use axum::Json;
use axum::extract::{Path, Query, State};
use owlee_errors::normalize_event;
use owlee_platform::Id;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::filter::{FilterInfo, InboundFilter};
use super::store::{self, App, EventSummary, IssueRow, Visible};
use crate::error::AppError;
use crate::paging::PageParams;
use crate::state::AppState;
use crate::workspaces::{Member, ProjectFilter, require_project};

/// The apps this request may see. Resolve it once per request.
pub(super) async fn visible(state: &AppState, member: &Member) -> Result<Visible, AppError> {
    Visible::resolve(&state.db, member.workspace_id, member.limit.as_ref()).await
}

/// For routes that change an app by id: 404 when the token's limit hides it.
async fn require_visible_app(state: &AppState, member: &Member, id: &str) -> Result<(), AppError> {
    if member.limit.is_none() {
        return Ok(());
    }
    let app = store::get_app(&state.db, member.workspace_id, id).await?;
    visible(state, member).await?.require(&app.id)
}

/// For routes that change an issue by id or short id: 404 when the token's limit hides it.
async fn require_visible_issue(
    state: &AppState,
    member: &Member,
    id: &str,
) -> Result<(), AppError> {
    if member.limit.is_none() {
        return Ok(());
    }
    let issue = store::get_issue(&state.db, member.workspace_id, id).await?;
    visible(state, member).await?.require(&issue.app_id)
}

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
    pub intake: crate::intake::IntakeCounts,
    pub allowed_domains: Vec<String>,
    pub inbound_filters: Vec<&'static str>,
    pub filter_options: Vec<FilterInfo>,
    pub spike_protection: bool,
    pub spike_limit: i64,
    pub used_this_hour: i64,
    pub replays: ReplaySettingsView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySettingsView {
    pub enabled: bool,
    pub daily_bytes: i64,
    pub hourly_limit: i64,
    pub today_bytes: i64,
    pub hour_replays: i64,
    /// Compressed bytes of every stored segment of the app.
    pub total_bytes: i64,
}

pub async fn list_apps(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<AppView>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let visible = visible(&state, &member).await?;
    let apps = store::list_apps(&state.db, member.workspace_id, project).await?;
    let mut views = Vec::with_capacity(apps.len());
    for app in apps.into_iter().filter(|app| visible.allows(&app.id)) {
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
    if let Some(limit) = &member.limit {
        if !limit.projects.contains(&body.project_id) {
            return Err(AppError::ResourceNotAllowed);
        }
    }
    let app = store::create_app(&state.db, body.project_id, &body.name, &body.platform).await?;
    Ok(Json(app_view(&state, app).await?))
}

pub async fn get_app(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<AppView>, AppError> {
    let app = store::get_app(&state.db, member.workspace_id, &id).await?;
    visible(&state, &member).await?.require(&app.id)?;
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
    require_visible_app(&state, &member, &id).await?;
    let app = store::set_daily_quota(&state.db, member.workspace_id, &id, body.daily_quota).await?;
    Ok(Json(app_view(&state, app).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectionBody {
    pub allowed_domains: Vec<String>,
    pub inbound_filters: Vec<String>,
    pub spike_protection: bool,
}

const MAX_ALLOWED_DOMAINS: usize = 50;

pub async fn set_app_protection(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Json(body): Json<ProtectionBody>,
) -> Result<Json<AppView>, AppError> {
    member.require_editor()?;
    let domains = parse_domains(&body.allowed_domains)?;
    let mut filters = Vec::new();
    for raw in &body.inbound_filters {
        let filter = InboundFilter::from_id(raw)
            .ok_or_else(|| AppError::field("inbound_filters", "Unknown inbound filter."))?;
        if !filters.contains(&filter) {
            filters.push(filter);
        }
    }
    require_visible_app(&state, &member, &id).await?;
    let app = store::set_protection(
        &state.db,
        member.workspace_id,
        &id,
        &domains,
        &filters,
        body.spike_protection,
    )
    .await?;
    Ok(Json(app_view(&state, app).await?))
}

/// URLs are cut to their host, so a pasted `https://shop.example.com/path` is stored as `shop.example.com`.
fn parse_domains(raw: &[String]) -> Result<Vec<String>, AppError> {
    let mut domains: Vec<String> = Vec::new();
    for entry in raw
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
    {
        let host = if entry == "*" {
            Ok("*".to_owned())
        } else {
            let bare = entry.split_once("://").map_or(entry, |(_, rest)| rest);
            owlee_insights::clean_host(bare.split(['/', '?', '#']).next().unwrap_or(""))
        };
        let host = host.map_err(|message| AppError::field("allowed_domains", message))?;
        if !domains.contains(&host) {
            domains.push(host);
        }
    }
    if domains.len() > MAX_ALLOWED_DOMAINS {
        return Err(AppError::field(
            "allowed_domains",
            "Use at most 50 allowed domains.",
        ));
    }
    Ok(domains)
}

pub async fn delete_app(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    member.require_editor()?;
    require_visible_app(&state, &member, &id).await?;
    store::delete_app(&state.db, member.workspace_id, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn app_view(state: &AppState, app: App) -> Result<AppView, AppError> {
    let global = crate::quota::error_quota(&state.db).await?;
    let daily_quota = crate::quota::effective(global, app.daily_quota);
    let used_today = crate::quota::used(&state.db, crate::quota::ERROR, &app.id).await?;
    let activity = store::app_activity(&state.db, &app.id).await?;
    let hour = crate::quota::current_hour();
    let used_this_hour = crate::quota::hour_used(state.db.pool(), &app.id, hour).await?;
    let (sum, sum_sq) = crate::quota::hour_baseline(state.db.pool(), &app.id, hour).await?;
    let replay_usage = super::replay_store::usage(&state.db, &app.id).await?;
    Ok(AppView {
        replays: ReplaySettingsView {
            enabled: app.replays_enabled,
            daily_bytes: app.replay_daily_bytes,
            hourly_limit: app.replay_hourly_limit,
            today_bytes: replay_usage.today_bytes,
            hour_replays: replay_usage.hour_replays,
            total_bytes: replay_usage.total_bytes,
        },
        allowed_domains: app.allowed_domains,
        inbound_filters: app
            .inbound_filters
            .iter()
            .map(|filter| filter.id())
            .collect(),
        filter_options: InboundFilter::ALL.map(InboundFilter::info).into(),
        spike_protection: app.spike_protection,
        spike_limit: crate::quota::spike_limit(sum, sum_sq, daily_quota),
        used_this_hour,
        dsn: store::build_dsn(&state.config.public_origin, &app.public_key, &app.id),
        quota_exceeded: used_today >= daily_quota,
        quota_override: app.daily_quota,
        resets_at: crate::quota::resets_at(),
        daily_quota,
        used_today,
        issue_count: activity.issue_count,
        event_count: activity.event_count,
        last_event_at: activity.last_event_at,
        intake: state.error_intake.get(&app.id),
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
    #[serde(default)]
    pub order: String,
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
    pub next_cursor: Option<String>,
}

/// The largest page of issues one request returns.
const ISSUE_PAGE: i64 = 50;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: IssueRow,
    pub latest: Option<Value>,
    pub first_event_id: Option<String>,
    pub latest_event_id: Option<String>,
    /// Every environment in the issue's app, so the filter keeps its options while filtered.
    pub environments: Vec<String>,
    pub histogram: Vec<i64>,
    pub tags: Vec<store::TagGroup>,
    pub replay_event_count: i64,
}

pub async fn list_issues(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<IssueFilter>,
    Query(project): Query<ProjectFilter>,
    Query(page): Query<PageParams>,
) -> Result<Json<IssueFeed>, AppError> {
    let paging = page.parse(ISSUE_PAGE)?;
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
    let visible = visible(&state, &member).await?;
    let environments = store::list_environments(
        &state.db,
        member.workspace_id,
        &visible,
        &project_id,
        &filter.app_id,
    )
    .await?;
    let page = store::list_issues(
        &state.db,
        &store::IssueQuery {
            workspace_id: member.workspace_id,
            visible: &visible,
            project_id,
            app_id: filter.app_id,
            status,
            search: filter.q,
            environment: filter.environment,
            since: filter.since,
            sort: filter.sort,
            order: filter.order,
        },
        &paging,
    )
    .await?;
    Ok(Json(IssueFeed {
        issues: page.issues,
        environments,
        counts: page.counts,
        next_cursor: page.next_cursor,
    }))
}

pub async fn get_issue(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Query(filter): Query<DetailFilter>,
) -> Result<Json<IssueDetail>, AppError> {
    let mut issue = store::get_issue(&state.db, member.workspace_id, &id).await?;
    let visible = visible(&state, &member).await?;
    visible.require(&issue.app_id)?;
    let filter = filter.events();
    let analytics = store::issue_analytics(
        &state.db,
        &issue.id,
        filter.range,
        filter.environment,
        filter.search,
    )
    .await?;
    let (first_event_id, latest_event_id) = store::edge_event_ids(&state.db, &issue.id).await?;
    let environments =
        store::list_environments(&state.db, member.workspace_id, &visible, "", &issue.app_id)
            .await?;
    let replay_event_count =
        super::replay_store::issue_replay_event_count(&state.db, &issue.id).await?;
    let latest = match &analytics.latest_payload {
        Some(payload) => Some(
            event_with_replay(
                &state,
                &member,
                &issue.app_id,
                analytics.latest_replay_id.as_deref(),
                analytics.latest_trace_id.as_deref(),
                payload,
            )
            .await?,
        ),
        None => None,
    };
    issue.period_event_count = analytics.period_events;
    issue.user_count = analytics.users;
    Ok(Json(IssueDetail {
        issue,
        latest,
        first_event_id,
        latest_event_id,
        environments,
        histogram: analytics.histogram,
        tags: analytics.tags,
        replay_event_count,
    }))
}

/// The largest page of an issue's events one request returns.
const EVENT_PAGE: i64 = 50;

impl DetailFilter {
    fn events(&self) -> store::EventFilter<'_> {
        store::EventFilter {
            range: if self.range.is_empty() {
                "14d"
            } else {
                &self.range
            },
            environment: &self.environment,
            search: &self.q,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPage {
    pub items: Vec<EventSummary>,
    pub next_cursor: Option<String>,
}

/// An issue's events, newest first, under the detail page's filters.
pub async fn issue_events(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Query(filter): Query<DetailFilter>,
    Query(page): Query<PageParams>,
) -> Result<Json<EventPage>, AppError> {
    let paging = page.parse(EVENT_PAGE)?;
    let issue = store::get_issue(&state.db, member.workspace_id, &id).await?;
    visible(&state, &member).await?.require(&issue.app_id)?;
    let events = store::issue_events(&state.db, &issue.id, &filter.events(), &paging).await?;
    Ok(Json(EventPage {
        items: events.items,
        next_cursor: events.next_cursor,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventNeighbors {
    pub older_id: Option<String>,
    pub newer_id: Option<String>,
}

/// The events next to one event, under the detail page's filters, so
/// Older and Newer reach every event and not only the loaded page.
pub async fn event_neighbors(
    State(state): State<AppState>,
    member: Member,
    Path((id, event_id)): Path<(String, String)>,
    Query(filter): Query<DetailFilter>,
) -> Result<Json<EventNeighbors>, AppError> {
    let issue = store::get_issue(&state.db, member.workspace_id, &id).await?;
    visible(&state, &member).await?.require(&issue.app_id)?;
    let (older_id, newer_id) =
        store::event_neighbors(&state.db, &issue.id, &event_id, &filter.events()).await?;
    Ok(Json(EventNeighbors { older_id, newer_id }))
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
    let visible = visible(&state, &member).await?;
    let updated = store::update_many_status(
        &state.db,
        member.workspace_id,
        &visible,
        &body.ids,
        &body.status,
    )
    .await?;
    Ok(Json(serde_json::json!({ "updated": updated })))
}

pub async fn update_issue(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Json(body): Json<StatusBody>,
) -> Result<Json<IssueRow>, AppError> {
    member.require_editor()?;
    require_visible_issue(&state, &member, &id).await?;
    Ok(Json(
        store::update_issue_status(&state.db, member.workspace_id, &id, &body.status).await?,
    ))
}

pub async fn get_event(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let visible = visible(&state, &member).await?;
    let event = store::get_event(&state.db, member.workspace_id, &visible, &id).await?;
    Ok(Json(
        event_with_replay(
            &state,
            &member,
            &event.app_id,
            event.replay_id.as_deref(),
            event.trace_id.as_deref(),
            &event.payload,
        )
        .await?,
    ))
}

/// The normalized event with `replay`, the linked replay when it exists in the same app, else
/// null, and `traceSpans`, whether a source of the workspace has spans of the event's trace.
async fn event_with_replay(
    state: &AppState,
    member: &Member,
    app_id: &str,
    replay_id: Option<&str>,
    trace_id: Option<&str>,
    payload: &str,
) -> Result<Value, AppError> {
    let replay = match replay_id {
        Some(replay_id) => super::replay_store::started_ms_of(&state.db, app_id, replay_id)
            .await?
            .map(|started_ms| serde_json::json!({ "id": replay_id, "startedMs": started_ms })),
        None => None,
    };
    let mut body = normalized_payload(payload);
    if let Value::Object(object) = &mut body {
        object.insert("replay".to_owned(), replay.unwrap_or(Value::Null));
        let trace_spans = match trace_id {
            Some(trace_id) => {
                crate::telemetry::trace_has_spans(&state.db, member.workspace_id, trace_id).await?
            }
            None => false,
        };
        object.insert("traceSpans".to_owned(), Value::Bool(trace_spans));
    }
    Ok(body)
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
    let replays =
        super::replay_store::replays_for_user(&state.db, member.workspace_id, &key).await?;
    Ok(Json(
        serde_json::json!({ "user": query.user.trim(), "events": events, "replays": replays }),
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
    super::replay_store::delete_replays_for_user(&state.db, member.workspace_id, &key).await?;
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
