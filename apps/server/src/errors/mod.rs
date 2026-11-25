//! Sentry-compatible error tracking. Dashboard routes are scoped to the
//! member's workspace. Ingest routes keep the Sentry SDK paths and take the
//! DSN key instead of a login.

mod alert;
mod filter;
mod http;
mod ingest;
mod replay;
mod replay_http;
mod replay_store;
mod seed;
mod store;

use axum::Router;
use axum::routing::{get, post, put};

use crate::state::AppState;

pub use alert::ISSUE_ALERTS_PER_HOUR;
pub use replay_store::{prune_replays, prune_usage as prune_replay_usage};
pub use seed::seed_demo;
pub use store::privatize_events;

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/error-apps", get(http::list_apps).post(http::create_app))
        .route(
            "/error-apps/{id}",
            get(http::get_app).delete(http::delete_app),
        )
        .route("/error-apps/{id}/quota", put(http::set_app_quota))
        .route("/error-apps/{id}/protection", put(http::set_app_protection))
        .route(
            "/error-apps/{id}/replays-settings",
            put(replay_http::set_settings),
        )
        .route("/error-apps/{id}/replays", get(replay_http::list))
        .route(
            "/error-apps/{id}/replays/{replay_id}",
            get(replay_http::detail).delete(replay_http::delete),
        )
        .route("/error-issues", get(http::list_issues))
        .route("/error-issues/bulk", post(http::bulk_update))
        .route(
            "/error-issues/{id}",
            get(http::get_issue).patch(http::update_issue),
        )
        .route("/error-issues/{id}/events", get(http::issue_events))
        .route(
            "/error-issues/{id}/events/{event_id}/neighbors",
            get(http::event_neighbors),
        )
        .route("/error-events/{id}", get(http::get_event))
}

/// Recordings need the `replays:read` scope. `errors:read` reaches the replay list and metadata only.
pub fn replay_recording_routes() -> Router<AppState> {
    Router::new().route(
        "/error-apps/{id}/replays/{replay_id}/segments/{segment_id}",
        get(replay_http::segment),
    )
}

/// Cookie only, unlike `api_routes`, so a token cannot export or erase a person's events.
pub fn privacy_routes() -> Router<AppState> {
    Router::new().route(
        "/privacy/errors",
        get(http::export_user_events).delete(http::delete_user_events),
    )
}

/// Sentry SDK paths. They take a DSN key, not the session cookie.
pub fn ingest_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/{app_id}/envelope",
            post(ingest::ingest_envelope).options(ingest::ingest_options),
        )
        .route(
            "/{app_id}/envelope/",
            post(ingest::ingest_envelope).options(ingest::ingest_options),
        )
        .route(
            "/{app_id}/store",
            post(ingest::ingest_store).options(ingest::ingest_options),
        )
        .route(
            "/{app_id}/store/",
            post(ingest::ingest_store).options(ingest::ingest_options),
        )
        .route(
            "/{app_id}/security/",
            post(ingest::ingest_security).options(ingest::ingest_options),
        )
}
