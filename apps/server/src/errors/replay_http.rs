//! Replay routes of an error app. Each loads the app through the workspace
//! and token checks, then reads only rows of that app.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use owlee_errors::RecordingEncoding;
use serde::{Deserialize, Serialize};

use super::http::{AppView, app_view, visible};
use super::replay_store::{self, ReplayDetail, ReplayFilter, ReplaySummary};
use super::store::{self, App};
use crate::error::AppError;
use crate::paging::PageParams;
use crate::state::AppState;
use crate::workspaces::Member;

const REPLAY_PAGE: i64 = 50;
const MIN_DAILY_BYTES: i64 = 1024 * 1024;
const MAX_DAILY_BYTES: i64 = 100 * 1024 * 1024 * 1024;
const MAX_HOURLY_LIMIT: i64 = 100_000;

async fn load_app(state: &AppState, member: &Member, id: &str) -> Result<App, AppError> {
    let app = store::get_app(&state.db, member.workspace_id, id).await?;
    visible(state, member).await?.require(&app.id)?;
    Ok(app)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySettingsBody {
    pub enabled: bool,
    pub daily_bytes: i64,
    pub hourly_limit: i64,
}

pub async fn set_settings(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Json(body): Json<ReplaySettingsBody>,
) -> Result<Json<AppView>, AppError> {
    member.require_editor()?;
    if !(MIN_DAILY_BYTES..=MAX_DAILY_BYTES).contains(&body.daily_bytes) {
        return Err(AppError::field(
            "daily_bytes",
            "Use a daily limit from 1 MiB to 100 GiB.",
        ));
    }
    if !(1..=MAX_HOURLY_LIMIT).contains(&body.hourly_limit) {
        return Err(AppError::field(
            "hourly_limit",
            "Use an hourly limit from 1 to 100000 new replays.",
        ));
    }
    let app = load_app(&state, &member, &id).await?;
    replay_store::set_settings(
        &state.db,
        &app.id,
        body.enabled,
        body.daily_bytes,
        body.hourly_limit,
    )
    .await?;
    let app = store::get_app(&state.db, member.workspace_id, &app.id).await?;
    Ok(Json(app_view(&state, app).await?))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReplayQuery {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub release: String,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub has_errors: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayPage {
    pub items: Vec<ReplaySummary>,
    pub next_cursor: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
    Query(query): Query<ReplayQuery>,
    Query(page): Query<PageParams>,
) -> Result<Json<ReplayPage>, AppError> {
    let paging = page.parse(REPLAY_PAGE)?;
    let app = load_app(&state, &member, &id).await?;
    let has_errors = match query.has_errors.as_deref() {
        None | Some("" | "false") => false,
        Some("true") => true,
        Some(_) => {
            return Err(AppError::BadRequest(
                "hasErrors must be true or false".into(),
            ));
        }
    };
    let filter = ReplayFilter {
        url: query.url,
        release: query.release,
        environment: query.environment,
        has_errors,
    };
    let page = replay_store::list(&state.db, &app.id, &filter, &paging).await?;
    Ok(Json(ReplayPage {
        items: page.items,
        next_cursor: page.next_cursor,
    }))
}

pub async fn detail(
    State(state): State<AppState>,
    member: Member,
    Path((id, replay_id)): Path<(String, String)>,
) -> Result<Json<ReplayDetail>, AppError> {
    let app = load_app(&state, &member, &id).await?;
    Ok(Json(
        replay_store::detail(&state.db, &app.id, &replay_id).await?,
    ))
}

/// The bytes as the SDK sent them. A zlib recording goes out as
/// `Content-Encoding: deflate`, so the browser inflates it.
pub async fn segment(
    State(state): State<AppState>,
    member: Member,
    Path((id, replay_id, segment_id)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let app = load_app(&state, &member, &id).await?;
    let segment_id: i64 = segment_id.parse().map_err(|_| AppError::NotFound)?;
    let (encoding, data) =
        replay_store::segment(&state.db, &app.id, &replay_id, segment_id).await?;
    let mut response = (StatusCode::OK, Body::from(data)).into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if encoding == RecordingEncoding::Deflate {
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("deflate"));
    }
    Ok(response)
}

pub async fn delete(
    State(state): State<AppState>,
    member: Member,
    Path((id, replay_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    let app = load_app(&state, &member, &id).await?;
    replay_store::delete(&state.db, &app.id, &replay_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
