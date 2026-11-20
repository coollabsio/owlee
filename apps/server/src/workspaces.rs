//! Workspaces, memberships, and projects, after Orbit's model. A user reaches a
//! workspace only through a membership row. Every query here filters by
//! workspace, and an outsider gets 404, never 403.

use std::ops::DerefMut;

use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, put};
use axum::{Json, Router};
use axum_extra::extract::cookie::CookieJar;
use owlee_platform::{Backend, Database, Id, TimestampMillis};
use serde::{Deserialize, Serialize};
use sqlx::{Any, Row, Transaction};

use crate::error::AppError;
use crate::repo::{User, millis, parse_id};
use crate::routes::require_user;
use crate::state::AppState;

pub const WORKSPACE_HEADER: &str = "x-owlee-workspace";

pub const DEFAULT_PROJECT_COLOR: &str = "#4f46e5";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    pub(crate) fn from_row(row: &sqlx::any::AnyRow) -> Result<Self, sqlx::Error> {
        match row.try_get::<String, _>("role")?.as_str() {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            other => Err(sqlx::Error::Decode(
                format!("unknown membership role {other}").into(),
            )),
        }
    }
}

/// The session user inside the workspace named by the `X-Owlee-Workspace` header.
///
/// 401 without a session. 404 `workspace_not_found` when the header is missing
/// or malformed, or the user is not a member, so an outsider cannot tell
/// whether a workspace exists. Every query a handler runs after this must
/// filter by `workspace_id`, and a record from another workspace is 404.
pub struct Member {
    pub user: User,
    pub workspace_id: Id,
    pub role: Role,
}

impl Member {
    /// Owners and admins change data. Members only read.
    #[must_use]
    pub const fn is_editor(&self) -> bool {
        matches!(self.role, Role::Owner | Role::Admin)
    }

    pub fn require_editor(&self) -> Result<(), AppError> {
        if self.is_editor() {
            Ok(())
        } else {
            Err(AppError::Forbidden(
                "Only a workspace admin can change this.",
            ))
        }
    }

    /// For lists only admins read, such as notification channels.
    pub fn require_editor_to_read(&self) -> Result<(), AppError> {
        if self.is_editor() {
            Ok(())
        } else {
            Err(AppError::Forbidden("Only a workspace admin can see this."))
        }
    }
}

impl FromRequestParts<AppState> for Member {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let jar = CookieJar::from_headers(&parts.headers);
        let user = require_user(state, &jar).await?;
        let workspace_id = parts
            .headers
            .get(WORKSPACE_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<Id>().ok())
            .ok_or(AppError::WorkspaceNotFound)?;
        let row =
            sqlx::query("SELECT role FROM memberships WHERE workspace_id = $1 AND user_id = $2")
                .bind(workspace_id.to_string())
                .bind(user.id.to_string())
                .fetch_optional(state.db.pool())
                .await?
                .ok_or(AppError::WorkspaceNotFound)?;
        Ok(Self {
            user,
            workspace_id,
            role: Role::from_row(&row)?,
        })
    }
}

/// 404 unless the project belongs to the workspace. Call it for the
/// `project_id` of every create body.
pub async fn require_project(
    db: &Database,
    workspace_id: Id,
    project_id: Id,
) -> Result<(), AppError> {
    sqlx::query("SELECT id FROM projects WHERE id = $1 AND workspace_id = $2")
        .bind(project_id.to_string())
        .bind(workspace_id.to_string())
        .fetch_optional(db.pool())
        .await?
        .map(|_| ())
        .ok_or(AppError::NotFound)
}

/// The optional `?project=<id>` of a list route. Take it as
/// `Query<ProjectFilter>` next to the route's own query struct.
#[derive(Debug, Default, Deserialize)]
pub struct ProjectFilter {
    project: Option<String>,
}

impl ProjectFilter {
    /// `None` lists the whole workspace. A malformed, unknown, or foreign id is 404.
    pub async fn resolve(&self, db: &Database, workspace_id: Id) -> Result<Option<Id>, AppError> {
        let Some(raw) = &self.project else {
            return Ok(None);
        };
        let project_id = raw.parse::<Id>().map_err(|_| AppError::NotFound)?;
        require_project(db, workspace_id, project_id).await?;
        Ok(Some(project_id))
    }
}

#[derive(Serialize)]
pub struct WorkspaceAccess {
    pub id: Id,
    pub name: String,
    pub role: Role,
    pub created_at: TimestampMillis,
}

#[derive(Serialize)]
struct MemberRow {
    id: Id,
    user_id: Id,
    email: String,
    display_name: String,
    role: Role,
    created_at: TimestampMillis,
}

#[derive(Serialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    pub color: String,
    pub created_at: TimestampMillis,
}

pub struct CreatedWorkspace {
    pub workspace_id: Id,
    pub project_id: Id,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/workspaces",
            get(list_workspaces).post(create_workspace_route),
        )
        .route("/workspace", put(rename_workspace).delete(delete_workspace))
        .route("/members", get(list_members))
        .route("/members/{id}", put(update_member).delete(remove_member))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", put(update_project).delete(delete_project))
}

pub async fn access_for_user(
    db: &Database,
    user_id: Id,
) -> Result<Vec<WorkspaceAccess>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT w.id, w.name, w.created_at, m.role FROM memberships m \
         JOIN workspaces w ON w.id = m.workspace_id \
         WHERE m.user_id = $1 ORDER BY w.created_at, w.id",
    )
    .bind(user_id.to_string())
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|row| {
            Ok(WorkspaceAccess {
                id: parse_id(row, "id")?,
                name: row.try_get("name")?,
                role: Role::from_row(row)?,
                created_at: millis(row, "created_at")?,
            })
        })
        .collect()
}

/// A workspace, its owner membership, and its first project.
pub async fn create_workspace(
    tx: &mut Transaction<'_, Any>,
    owner_id: Id,
    name: &str,
    project_name: &str,
    now: TimestampMillis,
) -> Result<CreatedWorkspace, sqlx::Error> {
    let workspace_id = Id::new();
    sqlx::query(
        "INSERT INTO workspaces (id, name, created_at, updated_at) VALUES ($1, $2, $3, $3)",
    )
    .bind(workspace_id.to_string())
    .bind(name)
    .bind(now.as_millis())
    .execute(tx.deref_mut())
    .await?;
    insert_membership(tx, workspace_id, owner_id, Role::Owner, now).await?;
    let project_id = Id::new();
    insert_project(
        tx,
        project_id,
        workspace_id,
        project_name,
        DEFAULT_PROJECT_COLOR,
        now,
    )
    .await?;
    Ok(CreatedWorkspace {
        workspace_id,
        project_id,
    })
}

/// Does nothing when the user is already a member.
pub async fn insert_membership(
    tx: &mut Transaction<'_, Any>,
    workspace_id: Id,
    user_id: Id,
    role: Role,
    now: TimestampMillis,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO memberships (id, workspace_id, user_id, role, created_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (workspace_id, user_id) DO NOTHING",
    )
    .bind(Id::new().to_string())
    .bind(workspace_id.to_string())
    .bind(user_id.to_string())
    .bind(role.as_str())
    .bind(now.as_millis())
    .execute(tx.deref_mut())
    .await?;
    Ok(())
}

pub async fn insert_project(
    tx: &mut Transaction<'_, Any>,
    id: Id,
    workspace_id: Id,
    name: &str,
    color: &str,
    now: TimestampMillis,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO projects (id, workspace_id, name, color, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5)",
    )
    .bind(id.to_string())
    .bind(workspace_id.to_string())
    .bind(name)
    .bind(color)
    .bind(now.as_millis())
    .execute(tx.deref_mut())
    .await?;
    Ok(())
}

/// An audit row scoped to a workspace. Failures are logged, not returned.
pub async fn audit(
    db: &Database,
    member: &Member,
    action: &str,
    resource_type: &str,
    resource_id: Option<Id>,
    now: TimestampMillis,
) {
    let result = sqlx::query(
        "INSERT INTO audit_events (id, workspace_id, actor_id, action, resource_type, resource_id, metadata_json, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, '{}', $7)",
    )
    .bind(Id::new().to_string())
    .bind(member.workspace_id.to_string())
    .bind(member.user.id.to_string())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id.map(|id| id.to_string()))
    .bind(now.as_millis())
    .execute(db.pool())
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, action, "writing an audit event failed");
    }
}

/// Serializes writers of one workspace on Postgres. SQLite's `BEGIN IMMEDIATE` already does.
pub(crate) async fn lock_workspace(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    workspace_id: Id,
) -> Result<(), sqlx::Error> {
    if db.backend() == Backend::Postgres {
        sqlx::query("SELECT id FROM workspaces WHERE id = $1 FOR UPDATE")
            .bind(workspace_id.to_string())
            .fetch_optional(tx.deref_mut())
            .await?;
    }
    Ok(())
}

async fn list_workspaces(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<Vec<WorkspaceAccess>>, AppError> {
    let user = require_user(&state, &jar).await?;
    Ok(Json(access_for_user(&state.db, user.id).await?))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn create_workspace_route(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<NameBody>,
) -> Result<(StatusCode, Json<WorkspaceAccess>), AppError> {
    let user = require_user(&state, &jar).await?;
    let name = name_field(&body.name)?;
    let now = TimestampMillis::now();
    let mut tx = state.db.begin_immediate().await?;
    let created = create_workspace(&mut tx, user.id, &name, "Default", now).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(WorkspaceAccess {
            id: created.workspace_id,
            name,
            role: Role::Owner,
            created_at: now,
        }),
    ))
}

async fn rename_workspace(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<NameBody>,
) -> Result<Json<WorkspaceAccess>, AppError> {
    member.require_editor()?;
    let name = name_field(&body.name)?;
    let now = TimestampMillis::now();
    let row = sqlx::query(
        "UPDATE workspaces SET name = $1, updated_at = $2 WHERE id = $3 RETURNING created_at",
    )
    .bind(&name)
    .bind(now.as_millis())
    .bind(member.workspace_id.to_string())
    .fetch_one(state.db.pool())
    .await?;
    audit(
        &state.db,
        &member,
        "workspace.renamed",
        "workspace",
        Some(member.workspace_id),
        now,
    )
    .await;
    Ok(Json(WorkspaceAccess {
        id: member.workspace_id,
        name,
        role: member.role,
        created_at: millis(&row, "created_at")?,
    }))
}

async fn delete_workspace(
    State(state): State<AppState>,
    member: Member,
) -> Result<StatusCode, AppError> {
    if member.role != Role::Owner {
        return Err(AppError::Forbidden(
            "Only the workspace owner can delete it.",
        ));
    }
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(member.workspace_id.to_string())
        .execute(state.db.pool())
        .await?;
    let _ = crate::repo::insert_audit(
        &state.db,
        Some(member.user.id),
        "workspace.deleted",
        "workspace",
        Some(&member.workspace_id.to_string()),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_members(
    State(state): State<AppState>,
    member: Member,
) -> Result<Json<Vec<MemberRow>>, AppError> {
    let rows = sqlx::query(
        "SELECT m.id, m.user_id, m.role, m.created_at, u.email, u.display_name FROM memberships m \
         JOIN users u ON u.id = m.user_id \
         WHERE m.workspace_id = $1 ORDER BY m.created_at, m.id",
    )
    .bind(member.workspace_id.to_string())
    .fetch_all(state.db.pool())
    .await?;
    let members = rows
        .iter()
        .map(member_row)
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Json(members))
}

fn member_row(row: &sqlx::any::AnyRow) -> Result<MemberRow, sqlx::Error> {
    Ok(MemberRow {
        id: parse_id(row, "id")?,
        user_id: parse_id(row, "user_id")?,
        email: row.try_get("email")?,
        display_name: row.try_get("display_name")?,
        role: Role::from_row(row)?,
        created_at: millis(row, "created_at")?,
    })
}

async fn membership(
    tx: &mut Transaction<'_, Any>,
    workspace_id: Id,
    id: Id,
) -> Result<MemberRow, AppError> {
    let row = sqlx::query(
        "SELECT m.id, m.user_id, m.role, m.created_at, u.email, u.display_name FROM memberships m \
         JOIN users u ON u.id = m.user_id WHERE m.id = $1 AND m.workspace_id = $2",
    )
    .bind(id.to_string())
    .bind(workspace_id.to_string())
    .fetch_optional(tx.deref_mut())
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(member_row(&row)?)
}

#[derive(Deserialize)]
struct RoleBody {
    role: Role,
}

async fn update_member(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<RoleBody>,
) -> Result<Json<MemberRow>, AppError> {
    member.require_editor()?;
    if body.role == Role::Owner {
        return Err(AppError::field("role", "Choose admin or member."));
    }
    let mut tx = state.db.begin_immediate().await?;
    let mut target = membership(&mut tx, member.workspace_id, id).await?;
    if target.role == Role::Owner {
        return Err(AppError::Conflict("The owner's role cannot change.".into()));
    }
    sqlx::query("UPDATE memberships SET role = $1 WHERE id = $2 AND workspace_id = $3")
        .bind(body.role.as_str())
        .bind(id.to_string())
        .bind(member.workspace_id.to_string())
        .execute(tx.deref_mut())
        .await?;
    tx.commit().await?;
    target.role = body.role;
    audit(
        &state.db,
        &member,
        "member.role_changed",
        "membership",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(Json(target))
}

/// An editor removes anyone but the owner. A member removes only themselves.
async fn remove_member(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    let mut tx = state.db.begin_immediate().await?;
    let target = membership(&mut tx, member.workspace_id, id).await?;
    if target.user_id != member.user.id {
        member.require_editor()?;
    }
    if target.role == Role::Owner {
        return Err(AppError::Conflict("The owner cannot be removed.".into()));
    }
    sqlx::query("DELETE FROM memberships WHERE id = $1 AND workspace_id = $2")
        .bind(id.to_string())
        .bind(member.workspace_id.to_string())
        .execute(tx.deref_mut())
        .await?;
    // Their pending links would let them back in after removal.
    sqlx::query(
        "UPDATE workspace_invitations SET revoked_at = $1 \
         WHERE workspace_id = $2 AND invited_by = $3 AND accepted_at IS NULL AND revoked_at IS NULL",
    )
    .bind(TimestampMillis::now().as_millis())
    .bind(member.workspace_id.to_string())
    .bind(target.user_id.to_string())
    .execute(tx.deref_mut())
    .await?;
    tx.commit().await?;
    audit(
        &state.db,
        &member,
        "member.removed",
        "membership",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_projects(
    State(state): State<AppState>,
    member: Member,
) -> Result<Json<Vec<Project>>, AppError> {
    Ok(Json(projects_in(&state.db, member.workspace_id).await?))
}

pub async fn projects_in(db: &Database, workspace_id: Id) -> Result<Vec<Project>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, name, color, created_at FROM projects WHERE workspace_id = $1 ORDER BY created_at, id",
    )
    .bind(workspace_id.to_string())
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|row| {
            Ok(Project {
                id: parse_id(row, "id")?,
                name: row.try_get("name")?,
                color: row.try_get("color")?,
                created_at: millis(row, "created_at")?,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct ProjectBody {
    name: String,
    color: Option<String>,
}

async fn create_project(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<ProjectBody>,
) -> Result<(StatusCode, Json<Project>), AppError> {
    member.require_editor()?;
    let name = name_field(&body.name)?;
    let color = color_field(body.color.as_deref())?;
    let now = TimestampMillis::now();
    let id = Id::new();
    let mut tx = state.db.begin_immediate().await?;
    lock_workspace(&mut tx, &state.db, member.workspace_id).await?;
    ensure_name_free(&mut tx, member.workspace_id, &name, None).await?;
    insert_project(&mut tx, id, member.workspace_id, &name, &color, now).await?;
    tx.commit().await?;
    audit(
        &state.db,
        &member,
        "project.created",
        "project",
        Some(id),
        now,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(Project {
            id,
            name,
            color,
            created_at: now,
        }),
    ))
}

async fn update_project(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<ProjectBody>,
) -> Result<Json<Project>, AppError> {
    member.require_editor()?;
    let name = name_field(&body.name)?;
    let color = color_field(body.color.as_deref())?;
    let now = TimestampMillis::now();
    let mut tx = state.db.begin_immediate().await?;
    lock_workspace(&mut tx, &state.db, member.workspace_id).await?;
    ensure_name_free(&mut tx, member.workspace_id, &name, Some(id)).await?;
    let row = sqlx::query(
        "UPDATE projects SET name = $1, color = $2, updated_at = $3 \
         WHERE id = $4 AND workspace_id = $5 RETURNING created_at",
    )
    .bind(&name)
    .bind(&color)
    .bind(now.as_millis())
    .bind(id.to_string())
    .bind(member.workspace_id.to_string())
    .fetch_optional(tx.deref_mut())
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;
    audit(
        &state.db,
        &member,
        "project.updated",
        "project",
        Some(id),
        now,
    )
    .await;
    Ok(Json(Project {
        id,
        name,
        color,
        created_at: millis(&row, "created_at")?,
    }))
}

async fn delete_project(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    let mut tx = state.db.begin_immediate().await?;
    lock_workspace(&mut tx, &state.db, member.workspace_id).await?;
    let rows = sqlx::query("SELECT id FROM projects WHERE workspace_id = $1")
        .bind(member.workspace_id.to_string())
        .fetch_all(tx.deref_mut())
        .await?;
    let mut found = false;
    for row in &rows {
        found |= parse_id(row, "id")? == id;
    }
    if !found {
        return Err(AppError::NotFound);
    }
    if rows.len() == 1 {
        return Err(AppError::Conflict(
            "A workspace keeps at least one project.".into(),
        ));
    }
    sqlx::query("DELETE FROM projects WHERE id = $1 AND workspace_id = $2")
        .bind(id.to_string())
        .bind(member.workspace_id.to_string())
        .execute(tx.deref_mut())
        .await?;
    tx.commit().await?;
    audit(
        &state.db,
        &member,
        "project.deleted",
        "project",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_name_free(
    tx: &mut Transaction<'_, Any>,
    workspace_id: Id,
    name: &str,
    except: Option<Id>,
) -> Result<(), AppError> {
    let row = sqlx::query("SELECT id FROM projects WHERE workspace_id = $1 AND name = $2")
        .bind(workspace_id.to_string())
        .bind(name)
        .fetch_optional(tx.deref_mut())
        .await?;
    match row {
        Some(row) if Some(parse_id(&row, "id")?) != except => Err(AppError::field(
            "name",
            "A project with this name already exists.",
        )),
        _ => Ok(()),
    }
}

fn name_field(raw: &str) -> Result<String, AppError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::field("name", "Use 1 to 80 characters."));
    }
    Ok(name.to_owned())
}

fn color_field(raw: Option<&str>) -> Result<String, AppError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_PROJECT_COLOR.to_owned());
    };
    let color = raw.trim().to_ascii_lowercase();
    let valid = color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(color)
    } else {
        Err(AppError::field("color", "Use a color like #4f46e5."))
    }
}
