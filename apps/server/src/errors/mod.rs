//! Sentry-compatible error tracking. Dashboard routes are scoped to the
//! member's workspace. Ingest routes keep the Sentry SDK paths and take the
//! DSN key instead of a login.

mod alert;
mod http;
mod ingest;
mod seed;
mod store;

use axum::Router;
use axum::routing::{get, post, put};

use crate::state::AppState;

pub use alert::ISSUE_ALERTS_PER_HOUR;
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
        .route(
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
