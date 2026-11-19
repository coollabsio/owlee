use axum::Json;
use axum::extract::{Path, Query, State};
use axum_extra::extract::cookie::CookieJar;
use owlee_errors::normalize_event;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::store::{self, EventSummary, IssueRow, Project};
use crate::error::AppError;
use crate::routes::{require_editor, require_user};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct NewProject {
    pub name: String,
    #[serde(default)]
    pub platform: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: String,
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

pub async fn list_projects(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<Vec<ProjectView>>, AppError> {
    require_user(&state, &jar).await?;
    let projects = store::list_projects(&state.db).await?;
    let mut views = Vec::with_capacity(projects.len());
    for project in projects {
        views.push(project_view(&state, project).await?);
    }
    Ok(Json(views))
}

pub async fn create_project(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<NewProject>,
) -> Result<Json<ProjectView>, AppError> {
    require_editor(&state, &jar).await?;
    let project = store::create_project(&state.db, &body.name, &body.platform).await?;
    Ok(Json(project_view(&state, project).await?))
}

pub async fn get_project(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<Json<ProjectView>, AppError> {
    require_user(&state, &jar).await?;
    Ok(Json(
        project_view(&state, store::get_project(&state.db, &id).await?).await?,
    ))
}

#[derive(Deserialize)]
pub struct QuotaBody {
    pub daily_quota: Option<i64>,
}

pub async fn set_project_quota(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
    Json(body): Json<QuotaBody>,
) -> Result<Json<ProjectView>, AppError> {
    require_editor(&state, &jar).await?;
    if let Some(quota) = body.daily_quota {
        if !(1..=100_000_000).contains(&quota) {
            return Err(AppError::field(
                "daily_quota",
                "Use a daily cap from 1 to 100000000, or leave it empty to inherit.",
            ));
        }
    }
    let project = store::set_daily_quota(&state.db, &id, body.daily_quota).await?;
    Ok(Json(project_view(&state, project).await?))
}

pub async fn delete_project(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    require_editor(&state, &jar).await?;
    store::delete_project(&state.db, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn project_view(state: &AppState, project: Project) -> Result<ProjectView, AppError> {
    let global = crate::quota::error_quota(&state.db).await?;
    let daily_quota = crate::quota::effective(global, project.daily_quota);
    let used_today = crate::quota::used(&state.db, crate::quota::ERROR, &project.id).await?;
    let activity = store::project_activity(&state.db, &project.id).await?;
    Ok(ProjectView {
        dsn: store::build_dsn(
            &state.config.public_origin,
            &project.public_key,
            &project.id,
        ),
        quota_exceeded: used_today >= daily_quota,
        quota_override: project.daily_quota,
        resets_at: crate::quota::resets_at(),
        daily_quota,
        used_today,
        issue_count: activity.issue_count,
        event_count: activity.event_count,
        last_event_at: activity.last_event_at,
        id: project.id,
        name: project.name,
        slug: project.slug,
        platform: project.platform,
        public_key: project.public_key,
        created_at: project.created_at,
    })
}

#[derive(Deserialize, Default)]
pub struct IssueFilter {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub project_id: String,
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
    /// Every environment in the issue's project, so the filter keeps its options while filtered.
    pub environments: Vec<String>,
    pub histogram: Vec<i64>,
    pub tags: Vec<store::TagGroup>,
}

pub async fn list_issues(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(filter): Query<IssueFilter>,
) -> Result<Json<IssueFeed>, AppError> {
    require_user(&state, &jar).await?;
    let status = match filter.status.as_str() {
        "" | "all" => String::new(),
        "unresolved" | "resolved" | "ignored" => filter.status,
        _ => {
            return Err(AppError::BadRequest(
                "Status filter must be unresolved, resolved, ignored, or all".into(),
            ));
        }
    };
    let environments = store::list_environments(&state.db, &filter.project_id).await?;
    let page = store::list_issues(
        &state.db,
        &store::IssueQuery {
            status,
            project_id: filter.project_id,
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
    jar: CookieJar,
    Path(id): Path<String>,
    Query(filter): Query<DetailFilter>,
) -> Result<Json<IssueDetail>, AppError> {
    require_user(&state, &jar).await?;
    let mut issue = store::get_issue(&state.db, &id).await?;
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
    let environments = store::list_environments(&state.db, &issue.project_id).await?;
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
    jar: CookieJar,
    Json(body): Json<BulkBody>,
) -> Result<Json<Value>, AppError> {
    require_editor(&state, &jar).await?;
    let updated = store::update_many_status(&state.db, &body.ids, &body.status).await?;
    Ok(Json(serde_json::json!({ "updated": updated })))
}

pub async fn update_issue(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
    Json(body): Json<StatusBody>,
) -> Result<Json<IssueRow>, AppError> {
    require_editor(&state, &jar).await?;
    Ok(Json(
        store::update_issue_status(&state.db, &id, &body.status).await?,
    ))
}

pub async fn get_event(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    require_user(&state, &jar).await?;
    let payload = store::get_event(&state.db, &id).await?;
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

/// Everything stored for one error-tracking user, found by the id, email,
/// username, or IP address the SDK sent. Admins only: payloads hold personal data.
pub async fn export_user_events(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<PrivacyQuery>,
) -> Result<Json<Value>, AppError> {
    require_editor(&state, &jar).await?;
    let key = privacy_key(&state, &query)?;
    let events = store::events_for_user(&state.db, &key).await?;
    Ok(Json(
        serde_json::json!({ "user": query.user.trim(), "events": events }),
    ))
}

pub async fn delete_user_events(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<PrivacyQuery>,
) -> Result<Json<Value>, AppError> {
    require_editor(&state, &jar).await?;
    let key = privacy_key(&state, &query)?;
    let deleted = store::delete_events_for_user(&state.db, &key).await?;
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
