//! OpenTelemetry sources, OTLP/HTTP intake, and logs. Dashboard routes take
//! the session cookie and are scoped to the member's workspace. The intake
//! routes take a source's bearer token.

mod logs;
mod otlp;
mod sources;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub use logs::prune as prune_logs;

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/telemetry-sources",
            get(sources::list_sources).post(sources::create_source),
        )
        .route(
            "/telemetry-sources/{id}",
            axum::routing::patch(sources::update_source).delete(sources::delete_source),
        )
        .route(
            "/telemetry-sources/{id}/rotate",
            post(sources::rotate_token),
        )
        .route("/logs", get(logs::search))
        .route("/logs/facets", get(logs::facets))
        .route("/logs/{id}", get(logs::detail))
}

/// `OTEL_EXPORTER_OTLP_ENDPOINT` is `https://<host>/otlp` (D6).
pub fn otlp_routes() -> Router<AppState> {
    Router::new()
        .route("/otlp/v1/logs", post(otlp::logs))
        .route("/otlp/v1/traces", post(otlp::traces))
        .route("/otlp/v1/metrics", post(otlp::metrics))
}
