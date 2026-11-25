//! OTLP sources: one bearer token each, stored as its SHA-256 (D5).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use owlee_platform::{Database, Id, TimestampMillis, generate_token, hash_token};
use owlee_telemetry::Signal;
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::Row;
use sqlx::any::AnyRow;

use crate::error::AppError;
use crate::intake::IntakeCounts;
use crate::state::AppState;
use crate::workspaces::{Member, ProjectFilter, require_project};

/// Telemetry tokens never start with `owl_`, the API token prefix.
const TOKEN_PREFIX: &str = "otlp_";
const MAX_NAME_CHARS: usize = 120;
const MAX_DAILY_QUOTA: i64 = 1_000_000_000;

/// The `IntakeStats` key of one source and signal.
pub(super) fn intake_key(source_id: &str, signal: Signal) -> String {
    let signal = match signal {
        Signal::Logs => "logs",
        Signal::Traces => "traces",
        Signal::Metrics => "metrics",
    };
    format!("{source_id}:{signal}")
}

pub(super) struct Source {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub token_hint: String,
    pub daily_quota: Option<i64>,
    pub created_at: i64,
    pub first_log_at: Option<i64>,
}

const COLUMNS: &str =
    "s.id, s.project_id, s.name, s.token_hint, s.daily_quota, s.created_at, s.first_log_at";

fn source_from_row(row: &AnyRow) -> Result<Source, sqlx::Error> {
    Ok(Source {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        name: row.try_get("name")?,
        token_hint: row.try_get("token_hint")?,
        daily_quota: row.try_get("daily_quota")?,
        created_at: row.try_get("created_at")?,
        first_log_at: row.try_get("first_log_at")?,
    })
}

/// The live sources of a workspace, optionally of one project in it.
pub(super) async fn list(
    db: &Database,
    workspace_id: Id,
    project_id: Option<Id>,
) -> Result<Vec<Source>, AppError> {
    let project = project_id.map(|id| id.to_string()).unwrap_or_default();
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM telemetry_sources s JOIN projects p ON p.id = s.project_id \
         WHERE p.workspace_id = $1 AND ($2 = '' OR s.project_id = $2) AND s.deleted_at IS NULL \
         ORDER BY s.created_at, s.id"
    ))
    .bind(workspace_id.to_string())
    .bind(project)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.iter().map(source_from_row).collect::<Result<_, _>>()?)
}

/// 404 unless the source is live and in the workspace.
async fn get(db: &Database, workspace_id: Id, id: &str) -> Result<Source, AppError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM telemetry_sources s JOIN projects p ON p.id = s.project_id \
         WHERE s.id = $1 AND p.workspace_id = $2 AND s.deleted_at IS NULL"
    ))
    .bind(id)
    .bind(workspace_id.to_string())
    .fetch_optional(db.pool())
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(source_from_row(&row)?)
}

/// The live source a bearer token belongs to.
pub(super) async fn for_token(db: &Database, token: &str) -> Result<Option<Source>, sqlx::Error> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM telemetry_sources s WHERE s.token_hash = $1 AND s.deleted_at IS NULL"
    ))
    .bind(hash_token(token))
    .fetch_optional(db.pool())
    .await?;
    row.as_ref().map(source_from_row).transpose()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub token_hint: String,
    pub daily_quota: Option<i64>,
    pub created_at: TimestampMillis,
    pub first_log_at: Option<TimestampMillis>,
    pub intake: IntakeView,
}

#[derive(Serialize)]
pub struct IntakeView {
    pub logs: IntakeCounts,
    pub traces: IntakeCounts,
    pub metrics: IntakeCounts,
}

#[derive(Serialize)]
pub struct WithToken<T> {
    #[serde(flatten)]
    pub view: T,
    pub token: String,
}

fn view(state: &AppState, source: Source) -> SourceView {
    let intake = |signal| state.telemetry_intake.get(&intake_key(&source.id, signal));
    SourceView {
        intake: IntakeView {
            logs: intake(Signal::Logs),
            traces: intake(Signal::Traces),
            metrics: intake(Signal::Metrics),
        },
        created_at: TimestampMillis::from_millis(source.created_at),
        first_log_at: source.first_log_at.map(TimestampMillis::from_millis),
        id: source.id,
        project_id: source.project_id,
        name: source.name,
        token_hint: source.token_hint,
        daily_quota: source.daily_quota,
    }
}

fn new_token() -> (String, String) {
    let token = format!("{TOKEN_PREFIX}{}", generate_token());
    let hint = token[token.len() - 4..].to_owned();
    (token, hint)
}

fn clean_name(name: &str) -> Result<String, AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(AppError::field(
            "name",
            "Use a name of 1 to 120 characters.",
        ));
    }
    Ok(name.to_owned())
}

async fn require_free_name(
    db: &Database,
    project_id: &str,
    name: &str,
    except_id: &str,
) -> Result<(), AppError> {
    let taken = sqlx::query(
        "SELECT id FROM telemetry_sources WHERE project_id = $1 AND name = $2 AND id <> $3 AND deleted_at IS NULL",
    )
    .bind(project_id)
    .bind(name)
    .bind(except_id)
    .fetch_optional(db.pool())
    .await?;
    if taken.is_some() {
        return Err(AppError::field(
            "name",
            "A source with this name exists in the project.",
        ));
    }
    Ok(())
}

pub async fn list_sources(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<SourceView>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let sources = list(&state.db, member.workspace_id, project).await?;
    Ok(Json(
        sources
            .into_iter()
            .map(|source| view(&state, source))
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewSource {
    pub project_id: Id,
    pub name: String,
}

pub async fn create_source(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<NewSource>,
) -> Result<Json<WithToken<SourceView>>, AppError> {
    member.require_editor()?;
    require_project(&state.db, member.workspace_id, body.project_id).await?;
    let name = clean_name(&body.name)?;
    let project_id = body.project_id.to_string();
    require_free_name(&state.db, &project_id, &name, "").await?;
    let (token, token_hint) = new_token();
    let source = Source {
        id: Id::new().to_string(),
        project_id,
        name,
        token_hint,
        daily_quota: None,
        created_at: TimestampMillis::now().as_millis(),
        first_log_at: None,
    };
    sqlx::query(
        "INSERT INTO telemetry_sources (id, project_id, name, token_hash, token_hint, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&source.id)
    .bind(&source.project_id)
    .bind(&source.name)
    .bind(hash_token(&token))
    .bind(&source.token_hint)
    .bind(source.created_at)
    .execute(state.db.pool())
    .await?;
    Ok(Json(WithToken {
        view: view(&state, source),
        token,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePatch {
    pub name: Option<String>,
    /// Absent leaves the quota. `null` inherits the instance default.
    #[serde(default, deserialize_with = "present")]
    pub daily_quota: Option<Option<i64>>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<i64>>, D::Error> {
    Option::<i64>::deserialize(deserializer).map(Some)
}

pub async fn update_source(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Json(body): Json<SourcePatch>,
) -> Result<Json<SourceView>, AppError> {
    member.require_editor()?;
    let mut source = get(&state.db, member.workspace_id, &id).await?;
    if let Some(Some(quota)) = body.daily_quota
        && !(1..=MAX_DAILY_QUOTA).contains(&quota)
    {
        return Err(AppError::field(
            "dailyQuota",
            "Use a daily cap from 1 to 1000000000, or leave it empty to inherit.",
        ));
    }
    if let Some(name) = &body.name {
        source.name = clean_name(name)?;
        require_free_name(&state.db, &source.project_id, &source.name, &source.id).await?;
    }
    if let Some(quota) = body.daily_quota {
        source.daily_quota = quota;
    }
    sqlx::query("UPDATE telemetry_sources SET name = $1, daily_quota = $2 WHERE id = $3")
        .bind(&source.name)
        .bind(source.daily_quota)
        .bind(&source.id)
        .execute(state.db.pool())
        .await?;
    Ok(Json(view(&state, source)))
}

/// Intake stops at once. The retention task deletes the logs in batches, then the row.
pub async fn delete_source(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    let source = get(&state.db, member.workspace_id, &id).await?;
    sqlx::query(
        "UPDATE telemetry_sources SET deleted_at = $1 WHERE id = $2 AND deleted_at IS NULL",
    )
    .bind(TimestampMillis::now().as_millis())
    .bind(&source.id)
    .execute(state.db.pool())
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct Rotated {
    pub token: String,
}

pub async fn rotate_token(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<Rotated>, AppError> {
    member.require_editor()?;
    let source = get(&state.db, member.workspace_id, &id).await?;
    let (token, hint) = new_token();
    sqlx::query("UPDATE telemetry_sources SET token_hash = $1, token_hint = $2 WHERE id = $3")
        .bind(hash_token(&token))
        .bind(hint)
        .bind(&source.id)
        .execute(state.db.pool())
        .await?;
    Ok(Json(Rotated { token }))
}
