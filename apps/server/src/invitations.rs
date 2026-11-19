//! Workspace invitations. The link carries a random token and only its hash is
//! stored. Accepting needs a signed-in account whose email matches the invite.

use std::ops::DerefMut;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::CookieJar;
use owlee_platform::{Id, TimestampMillis, generate_token, hash_token, normalize_email};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;

use crate::error::AppError;
use crate::notify;
use crate::repo::{millis, parse_id};
use crate::routes::{mail_allowed, request_email, require_user};
use crate::state::AppState;
use crate::workspaces::{Member, Role, audit, insert_membership, lock_workspace};

const INVITATION_LIFETIME_MS: i64 = 7 * 86_400_000;

const PENDING: &str = "accepted_at IS NULL AND revoked_at IS NULL";

#[derive(Serialize)]
struct Invitation {
    id: Id,
    email: String,
    role: Role,
    expires_at: TimestampMillis,
    created_at: TimestampMillis,
    /// Only in the answer to the create call. The token cannot be read back later.
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/invitations", get(list).post(create))
        // `id` is an invitation id for DELETE and the link token for GET.
        .route("/invitations/{id}", get(preview).delete(revoke))
        .route("/invitations/{id}/accept", post(accept))
}

async fn list(
    State(state): State<AppState>,
    member: Member,
) -> Result<Json<Vec<Invitation>>, AppError> {
    member.require_editor()?;
    let rows = sqlx::query(&format!(
        "SELECT id, email, role, expires_at, created_at FROM workspace_invitations \
         WHERE workspace_id = $1 AND {PENDING} AND expires_at > $2 ORDER BY created_at, id"
    ))
    .bind(member.workspace_id.to_string())
    .bind(TimestampMillis::now().as_millis())
    .fetch_all(state.db.pool())
    .await?;
    let invitations = rows
        .iter()
        .map(|row| {
            Ok(Invitation {
                id: parse_id(row, "id")?,
                email: row.try_get("email")?,
                role: Role::from_row(row)?,
                expires_at: millis(row, "expires_at")?,
                created_at: millis(row, "created_at")?,
                url: None,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Json(invitations))
}

#[derive(Deserialize)]
struct CreateBody {
    email: String,
    role: Role,
}

/// Inviting an address again revokes its earlier link.
async fn create(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Invitation>), AppError> {
    member.require_editor()?;
    if body.role == Role::Owner {
        return Err(AppError::field("role", "Choose admin or member."));
    }
    let normalized = request_email(&body.email)?;
    let email = body.email.trim().to_owned();
    let now = TimestampMillis::now();
    let expires_at = TimestampMillis::from_millis(now.as_millis() + INVITATION_LIFETIME_MS);
    let id = Id::new();
    let token = generate_token();
    let workspace = member.workspace_id.to_string();

    let mut tx = state.db.begin_immediate().await?;
    lock_workspace(&mut tx, &state.db, member.workspace_id).await?;
    let already = sqlx::query(
        "SELECT m.id FROM memberships m JOIN users u ON u.id = m.user_id \
         WHERE m.workspace_id = $1 AND u.normalized_email = $2",
    )
    .bind(&workspace)
    .bind(&normalized)
    .fetch_optional(tx.deref_mut())
    .await?;
    if already.is_some() {
        return Err(AppError::field(
            "email",
            "This person is already a member of the workspace.",
        ));
    }
    sqlx::query(&format!(
        "UPDATE workspace_invitations SET revoked_at = $1 \
         WHERE workspace_id = $2 AND normalized_email = $3 AND {PENDING}"
    ))
    .bind(now.as_millis())
    .bind(&workspace)
    .bind(&normalized)
    .execute(tx.deref_mut())
    .await?;
    sqlx::query(
        "INSERT INTO workspace_invitations \
            (id, workspace_id, email, normalized_email, role, token_hash, invited_by, expires_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id.to_string())
    .bind(&workspace)
    .bind(&email)
    .bind(&normalized)
    .bind(body.role.as_str())
    .bind(hash_token(&token))
    .bind(member.user.id.to_string())
    .bind(expires_at.as_millis())
    .bind(now.as_millis())
    .execute(tx.deref_mut())
    .await?;
    let name: String = sqlx::query("SELECT name FROM workspaces WHERE id = $1")
        .bind(&workspace)
        .fetch_one(tx.deref_mut())
        .await?
        .try_get("name")?;
    tx.commit().await?;

    let url = format!(
        "{}/invite/{token}",
        state.config.public_origin.trim_end_matches('/')
    );
    if notify::effective_smtp(&state).await.is_some() && mail_allowed(&state, &normalized) {
        let text = format!(
            "{} invited you to the {name} workspace on Owlee.\n\n\
             Open this link to join:\n\n{url}\n\n\
             The link works once, for 7 days, for an account with this email address. \
             If you did not expect it, ignore this email.",
            member.user.display_name
        );
        notify::send_account_mail(
            &state,
            &normalized,
            "You are invited to an Owlee workspace",
            text,
        )
        .await;
    }
    audit(
        &state.db,
        &member,
        "invitation.created",
        "invitation",
        Some(id),
        now,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(Invitation {
            id,
            email,
            role: body.role,
            expires_at,
            created_at: now,
            url: Some(url),
        }),
    ))
}

async fn revoke(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    let now = TimestampMillis::now();
    let revoked = sqlx::query(&format!(
        "UPDATE workspace_invitations SET revoked_at = $1 \
         WHERE id = $2 AND workspace_id = $3 AND {PENDING}"
    ))
    .bind(now.as_millis())
    .bind(id.to_string())
    .bind(member.workspace_id.to_string())
    .execute(state.db.pool())
    .await?;
    if revoked.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "invitation.revoked",
        "invitation",
        Some(id),
        now,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Shown before sign-in, so the page can say which account the link is for.
async fn preview(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Value>, AppError> {
    let row = sqlx::query(&format!(
        "SELECT i.email, i.role, w.name FROM workspace_invitations i \
         JOIN workspaces w ON w.id = i.workspace_id \
         WHERE i.token_hash = $1 AND {PENDING} AND i.expires_at > $2"
    ))
    .bind(hash_token(&token))
    .bind(TimestampMillis::now().as_millis())
    .fetch_optional(state.db.pool())
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(json!({
        "workspace_name": row.try_get::<String, _>("name")?,
        "email": row.try_get::<String, _>("email")?,
        "role": Role::from_row(&row)?,
    })))
}

async fn accept(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(token): Path<String>,
) -> Result<Json<Value>, AppError> {
    let user = require_user(&state, &jar).await?;
    let now = TimestampMillis::now();
    let mut tx = state.db.begin_immediate().await?;
    let row = sqlx::query(&format!(
        "SELECT id, workspace_id, normalized_email, role FROM workspace_invitations \
         WHERE token_hash = $1 AND {PENDING} AND expires_at > $2"
    ))
    .bind(hash_token(&token))
    .bind(now.as_millis())
    .fetch_optional(tx.deref_mut())
    .await?
    .ok_or(AppError::NotFound)?;
    let invited: String = row.try_get("normalized_email")?;
    if normalize_email(&user.email) != invited {
        return Err(AppError::Forbidden(
            "This invitation was sent to another email address.",
        ));
    }
    let id = parse_id(&row, "id")?;
    let workspace_id = parse_id(&row, "workspace_id")?;
    // The guard on the update makes a second accept of the same link lose.
    let claimed = sqlx::query(&format!(
        "UPDATE workspace_invitations SET accepted_at = $1 WHERE id = $2 AND {PENDING}"
    ))
    .bind(now.as_millis())
    .bind(id.to_string())
    .execute(tx.deref_mut())
    .await?;
    if claimed.rows_affected() != 1 {
        return Err(AppError::NotFound);
    }
    insert_membership(&mut tx, workspace_id, user.id, Role::from_row(&row)?, now).await?;
    tx.commit().await?;
    Ok(Json(json!({ "workspace_id": workspace_id })))
}
