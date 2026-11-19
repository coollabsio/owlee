use axum::Json;
use axum::extract::{Path, Query, State};
use owlee_errors::normalize_event;
use owlee_platform::Id;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::store::{self, App, EventSummary, IssueRow};
use crate::error::AppError;
use crate::state::AppState;
use crate::workspaces::{Member, ProjectFilter, require_project};

#[derive(Deserialize)]
pub struct NewApp {
    pub name: String,
    #[serde(default)]
    pub platform: String,
    pub project_id: Id,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    pub id: String,
    pub project_id: Id,
    pub name: String,
    pub slug: String,
    pub platform: String,
    pub public_key: String,
    pub dsn: String,
    pub created_at: String,
    pub daily_quota: i64,
    pub quota_override: Option<i64>,
    pub used_today: i64,
    pub quota_exceeded: bool,
    pub resets_at: String,
    pub issue_count: i64,
    pub event_count: i64,
    pub last_event_at: Option<String>,
}

pub async fn list_apps(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<AppView>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let apps = store::list_apps(&state.db, member.workspace_id, project).await?;
    let mut views = Vec::with_capacity(apps.len());
    for app in apps {
        views.push(app_view(&state, app).await?);
    }
    Ok(Json(views))
}

pub async fn create_app(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<NewApp>,
) -> Result<Json<AppView>, AppError> {
    member.require_editor()?;
    require_project(&state.db, member.workspace_id, body.project_id).await?;
    let app = store::create_app(&state.db, body.project_id, &body.name, &body.platform).await?;
    Ok(Json(app_view(&state, app).await?))
}

pub async fn get_app(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<AppView>, AppError> {
    let app = store::get_app(&state.db, member.workspace_id, &id).await?;
    Ok(Json(app_view(&state, app).await?))
}

#[derive(Deserialize)]
pub struct QuotaBody {
    pub daily_quota: Option<i64>,
}

pub async fn set_app_quota(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Json(body): Json<QuotaBody>,
) -> Result<Json<AppView>, AppError> {
    member.require_editor()?;
    if let Some(quota) = body.daily_quota {
        if !(1..=100_000_000).contains(&quota) {
            return Err(AppError::field(
                "daily_quota",
                "Use a daily cap from 1 to 100000000, or leave it empty to inherit.",
            ));
        }
    }
    let app = store::set_daily_quota(&state.db, member.workspace_id, &id, body.daily_quota).await?;
    Ok(Json(app_view(&state, app).await?))
}

pub async fn delete_app(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    member.require_editor()?;
    store::delete_app(&state.db, member.workspace_id, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn app_view(state: &AppState, app: App) -> Result<AppView, AppError> {
    let global = crate::quota::error_quota(&state.db).await?;
    let daily_quota = crate::quota::effective(global, app.daily_quota);
    let used_today = crate::quota::used(&state.db, crate::quota::ERROR, &app.id).await?;
    let activity = store::app_activity(&state.db, &app.id).await?;
    Ok(AppView {
        dsn: store::build_dsn(&state.config.public_origin, &app.public_key, &app.id),
        quota_exceeded: used_today >= daily_quota,
        quota_override: app.daily_quota,
        resets_at: crate::quota::resets_at(),
        daily_quota,
        used_today,
        issue_count: activity.issue_count,
        event_count: activity.event_count,
        last_event_at: activity.last_event_at,
        id: app.id,
        project_id: app.project_id,
        name: app.name,
        slug: app.slug,
        platform: app.platform,
        public_key: app.public_key,
        created_at: app.created_at,
    })
}

#[derive(Deserialize, Default)]
pub struct IssueFilter {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub since: String,
    #[serde(default)]
    pub sort: String,
}

#[derive(Deserialize, Default)]
pub struct DetailFilter {
    #[serde(default)]
    pub range: String,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub q: String,
}

#[derive(Deserialize)]
pub struct StatusBody {
    pub status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueFeed {
    pub issues: Vec<IssueRow>,
    pub environments: Vec<String>,
    pub counts: store::IssueCounts,
    pub truncated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: IssueRow,
    pub events: Vec<EventSummary>,
    pub latest: Option<Value>,
    pub first_event_id: Option<String>,
    pub latest_event_id: Option<String>,
    /// Every environment in the issue's app, so the filter keeps its options while filtered.
    pub environments: Vec<String>,
    pub histogram: Vec<i64>,
    pub tags: Vec<store::TagGroup>,
}

pub async fn list_issues(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<IssueFilter>,
    Query(project): Query<ProjectFilter>,
) -> Result<Json<IssueFeed>, AppError> {
    let project_id = project
        .resolve(&state.db, member.workspace_id)
        .await?
        .map(|id| id.to_string())
        .unwrap_or_default();
    let status = match filter.status.as_str() {
        "" | "all" => String::new(),
        "unresolved" | "resolved" | "ignored" => filter.status,
        _ => {
            return Err(AppError::BadRequest(
                "Status filter must be unresolved, resolved, ignored, or all".into(),
            ));
        }
    };
    let environments =
        store::list_environments(&state.db, member.workspace_id, &project_id, &filter.app_id)
            .await?;
    let page = store::list_issues(
        &state.db,
        &store::IssueQuery {
            workspace_id: member.workspace_id,
            project_id,
            app_id: filter.app_id,
            status,
            search: filter.q,
            environment: filter.environment,
            since: filter.since,
            sort: filter.sort,
        },
    )
    .await?;
    Ok(Json(IssueFeed {
        issues: page.issues,
        environments,
        counts: page.counts,
        truncated: page.truncated,
    }))
}

pub async fn get_issue(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Query(filter): Query<DetailFilter>,
) -> Result<Json<IssueDetail>, AppError> {
    let mut issue = store::get_issue(&state.db, member.workspace_id, &id).await?;
    let analytics = store::issue_analytics(
        &state.db,
        &issue.id,
        if filter.range.is_empty() {
            "14d"
        } else {
            &filter.range
        },
        &filter.environment,
        &filter.q,
    )
    .await?;
    let (first_event_id, latest_event_id) = store::edge_event_ids(&state.db, &issue.id).await?;
    let environments =
        store::list_environments(&state.db, member.workspace_id, "", &issue.app_id).await?;
    issue.period_event_count = analytics.period_events;
    issue.user_count = analytics.users;
    Ok(Json(IssueDetail {
        issue,
        events: analytics.events,
        latest: analytics
            .latest_payload
            .map(|payload| normalized_payload(&payload)),
        first_event_id,
        latest_event_id,
        environments,
        histogram: analytics.histogram,
        tags: analytics.tags,
    }))
}

#[derive(Deserialize)]
pub struct BulkBody {
    pub ids: Vec<String>,
    pub status: String,
}

pub async fn bulk_update(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<BulkBody>,
) -> Result<Json<Value>, AppError> {
    member.require_editor()?;
    let updated =
        store::update_many_status(&state.db, member.workspace_id, &body.ids, &body.status).await?;
    Ok(Json(serde_json::json!({ "updated": updated })))
}

pub async fn update_issue(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Json(body): Json<StatusBody>,
) -> Result<Json<IssueRow>, AppError> {
    member.require_editor()?;
    Ok(Json(
        store::update_issue_status(&state.db, member.workspace_id, &id, &body.status).await?,
    ))
}

pub async fn get_event(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let payload = store::get_event(&state.db, member.workspace_id, &id).await?;
    Ok(Json(normalized_payload(&payload)))
}

fn normalized_payload(payload: &str) -> Value {
    let raw = serde_json::from_str(payload).unwrap_or(Value::Null);
    serde_json::to_value(normalize_event(raw)).unwrap_or(Value::Null)
}

#[derive(Deserialize)]
pub struct PrivacyQuery {
    #[serde(default)]
    pub user: String,
}

/// Everything the workspace's apps stored for one error-tracking user, found by
/// the id, email, username, or IP address the SDK sent. Editors only: payloads
/// hold personal data.
pub async fn export_user_events(
    State(state): State<AppState>,
    member: Member,
    Query(query): Query<PrivacyQuery>,
) -> Result<Json<Value>, AppError> {
    member.require_editor()?;
    let key = privacy_key(&state, &query)?;
    let events = store::events_for_user(&state.db, member.workspace_id, &key).await?;
    Ok(Json(
        serde_json::json!({ "user": query.user.trim(), "events": events }),
    ))
}

pub async fn delete_user_events(
    State(state): State<AppState>,
    member: Member,
    Query(query): Query<PrivacyQuery>,
) -> Result<Json<Value>, AppError> {
    member.require_editor()?;
    let key = privacy_key(&state, &query)?;
    let deleted = store::delete_events_for_user(&state.db, member.workspace_id, &key).await?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

fn privacy_key(state: &AppState, query: &PrivacyQuery) -> Result<String, AppError> {
    let user = query.user.trim();
    if user.is_empty() {
        return Err(AppError::field(
            "user",
            "Enter a user id, email, username, or IP address.",
        ));
    }
    Ok(store::hash_user_key(&state.config.app_key, user))
}
