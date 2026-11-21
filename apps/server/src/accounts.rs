//! Accounts on the install: list, create, promote, disable, delete, and set a
//! password. The HTTP routes check who may act; the CLI calls the same
//! functions with host access instead of a session.

use std::ops::DerefMut;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use axum_extra::extract::cookie::CookieJar;
use owlee_platform::{Id, TimestampMillis};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

use crate::error::AppError;
use crate::instance_settings;
use crate::repo::{self, Account, User};
use crate::routes::{account_fields, password_field, require_installation_admin, require_root};
use crate::state::{self, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/users", get(list).post(create))
        .route("/admin/users/{id}", axum::routing::delete(delete))
        .route("/admin/users/{id}/admin", put(set_installation_admin))
        .route("/admin/users/{id}/disabled", put(set_disabled))
        .route("/admin/users/{id}/password", put(set_password))
}

async fn list(State(state): State<AppState>, jar: CookieJar) -> Result<Json<Value>, AppError> {
    require_installation_admin(&state, &jar).await?;
    let root_id = instance_settings::root_user_id(&state.db).await?;
    let accounts = repo::list_accounts(&state.db).await?;
    Ok(Json(json!(
        accounts
            .iter()
            .map(|account| account_body(account, root_id))
            .collect::<Vec<_>>()
    )))
}

#[derive(Deserialize)]
struct CreateBody {
    email: String,
    password: String,
    display_name: Option<String>,
}

/// New accounts are members, as with registration.
async fn create(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let actor = require_installation_admin(&state, &jar).await?;
    let now = TimestampMillis::now();
    let user = create_account(&state, &body.email, body.display_name, body.password, now).await?;
    audit(&state, actor.id, "user.created", user.id, now).await;
    Ok((StatusCode::CREATED, Json(body_for(&state, user.id).await?)))
}

#[derive(Deserialize)]
struct AdminBody {
    admin: bool,
}

async fn set_installation_admin(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<Id>,
    Json(body): Json<AdminBody>,
) -> Result<Json<Value>, AppError> {
    let actor = require_root(&state, &jar).await?;
    if instance_settings::root_user_id(&state.db).await? == Some(id) {
        return Err(AppError::Conflict(
            "The root user stays an installation admin.".into(),
        ));
    }
    let now = TimestampMillis::now();
    if !repo::set_installation_admin(&state.db, id, body.admin, now).await? {
        return Err(AppError::NotFound);
    }
    let action = if body.admin {
        "instance.admin_granted"
    } else {
        "instance.admin_revoked"
    };
    audit(&state, actor.id, action, id, now).await;
    Ok(Json(body_for(&state, id).await?))
}

#[derive(Deserialize)]
struct DisabledBody {
    disabled: bool,
}

async fn set_disabled(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<Id>,
    Json(body): Json<DisabledBody>,
) -> Result<Json<Value>, AppError> {
    let actor = require_installation_admin(&state, &jar).await?;
    let target = managed_target(&state, &actor, id).await?;
    let now = TimestampMillis::now();
    disable_account(&state, target.id, body.disabled, now).await?;
    let action = if body.disabled {
        "user.disabled"
    } else {
        "user.enabled"
    };
    audit(&state, actor.id, action, id, now).await;
    Ok(Json(body_for(&state, id).await?))
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

async fn set_password(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<Id>,
    Json(body): Json<PasswordBody>,
) -> Result<StatusCode, AppError> {
    let actor = require_installation_admin(&state, &jar).await?;
    let target = managed_target(&state, &actor, id).await?;
    let now = TimestampMillis::now();
    reset_password(&state, target.id, body.password, now).await?;
    audit(&state, actor.id, "user.password_set", id, now).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    let actor = require_installation_admin(&state, &jar).await?;
    let target = managed_target(&state, &actor, id).await?;
    let now = TimestampMillis::now();
    delete_account(&state, target.id, now).await?;
    audit(&state, actor.id, "user.deleted", id, now).await;
    Ok(StatusCode::NO_CONTENT)
}

/// The root account is never changed here, and only the root user changes an
/// installation admin, so one admin cannot lock out or take over another.
async fn managed_target(state: &AppState, actor: &User, id: Id) -> Result<User, AppError> {
    let root_id = instance_settings::root_user_id(&state.db).await?;
    if root_id == Some(id) {
        return Err(AppError::Conflict(
            "The root account cannot be changed.".into(),
        ));
    }
    let target = repo::user_by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if target.installation_admin && root_id != Some(actor.id) {
        return Err(AppError::Forbidden(
            "Only the root user can change an installation admin.",
        ));
    }
    Ok(target)
}

pub(crate) async fn create_account(
    state: &AppState,
    email: &str,
    display_name: Option<String>,
    password: String,
    now: TimestampMillis,
) -> Result<User, AppError> {
    let account = account_fields(email, display_name)?;
    state
        .passwords
        .validate(&password)
        .map_err(password_field)?;
    if repo::user_by_email(&state.db, &account.normalized)
        .await?
        .is_some()
    {
        return Err(AppError::field(
            "email",
            "An account with this email already exists.",
        ));
    }
    let hash = state::hash_password(state, password)
        .await
        .map_err(password_field)?;
    let id = Id::new();
    repo::insert_user(
        &state.db,
        id,
        &account.email,
        &account.normalized,
        &account.display_name,
        &hash,
        now,
    )
    .await?;
    repo::user_by_id(&state.db, id)
        .await?
        .ok_or(AppError::Internal)
}

/// Disabling signs the account out everywhere. Its data and memberships stay.
pub(crate) async fn disable_account(
    state: &AppState,
    id: Id,
    disabled: bool,
    now: TimestampMillis,
) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET disabled_at = $1, updated_at = $2 WHERE id = $3")
        .bind(disabled.then_some(now.as_millis()))
        .bind(now.as_millis())
        .bind(id.to_string())
        .execute(state.db.pool())
        .await?;
    if disabled {
        repo::revoke_user_sessions(&state.db, id, None, now).await?;
    }
    Ok(())
}

/// Signs the account out everywhere, so whoever had the old password loses access.
pub(crate) async fn reset_password(
    state: &AppState,
    id: Id,
    password: String,
    now: TimestampMillis,
) -> Result<(), AppError> {
    state
        .passwords
        .validate(&password)
        .map_err(password_field)?;
    let hash = state::hash_password(state, password)
        .await
        .map_err(password_field)?;
    repo::set_password(&state.db, id, &hash, now).await?;
    repo::revoke_user_sessions(&state.db, id, None, now).await?;
    Ok(())
}

/// A workspace keeps exactly one owner and ownership cannot move, so an owner
/// is refused. Their sessions, memberships, and sent invitations go with them.
async fn delete_account(state: &AppState, id: Id, now: TimestampMillis) -> Result<(), AppError> {
    repo::revoke_user_sessions(&state.db, id, None, now).await?;
    let mut tx = state.db.begin_immediate().await?;
    let owned = sqlx::query(
        "SELECT w.name FROM memberships m JOIN workspaces w ON w.id = m.workspace_id \
         WHERE m.user_id = $1 AND m.role = 'owner' ORDER BY w.name",
    )
    .bind(id.to_string())
    .fetch_all(tx.deref_mut())
    .await?;
    if !owned.is_empty() {
        let names = owned
            .iter()
            .map(|row| row.try_get::<String, _>("name"))
            .collect::<Result<Vec<_>, _>>()?;
        return Err(AppError::Conflict(format!(
            "This account owns {}: {}. Only the owner can delete a workspace, so disable the account instead.",
            if names.len() == 1 {
                "a workspace"
            } else {
                "workspaces"
            },
            names.join(", ")
        )));
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id.to_string())
        .execute(tx.deref_mut())
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn audit(state: &AppState, actor: Id, action: &str, target: Id, now: TimestampMillis) {
    let _ = repo::insert_audit(
        &state.db,
        Some(actor),
        action,
        "user",
        Some(&target.to_string()),
        now,
    )
    .await;
}

async fn body_for(state: &AppState, id: Id) -> Result<Value, AppError> {
    let root_id = instance_settings::root_user_id(&state.db).await?;
    let accounts = repo::list_accounts(&state.db).await?;
    let account = accounts
        .iter()
        .find(|account| account.id == id)
        .ok_or(AppError::NotFound)?;
    Ok(account_body(account, root_id))
}

fn account_body(account: &Account, root_id: Option<Id>) -> Value {
    let root = root_id == Some(account.id);
    json!({
        "id": account.id,
        "email": account.email,
        "display_name": account.display_name,
        "root": root,
        "installation_admin": root || account.installation_admin,
        "disabled": account.disabled,
    })
}
