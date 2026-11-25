//! OpenTelemetry sources, OTLP/HTTP intake, logs, traces, and their retention.
//! Dashboard routes take the session cookie and are scoped to the member's
//! workspace. The intake routes take a source's bearer token.

mod filter;
mod logs;
mod otlp;
mod sources;
mod traces;

use axum::Router;
use axum::routing::{get, post};
use owlee_platform::Database;
use sqlx::Row;

use crate::state::AppState;

pub use traces::trace_has_spans;

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
        .route("/services", get(traces::services))
        .route("/services/{service}", get(traces::service))
        .route("/traces", get(traces::list))
        .route("/traces/{trace_id}", get(traces::detail))
}

/// `OTEL_EXPORTER_OTLP_ENDPOINT` is `https://<host>/otlp` (D6).
pub fn otlp_routes() -> Router<AppState> {
    Router::new()
        .route("/otlp/v1/logs", post(otlp::logs))
        .route("/otlp/v1/traces", post(otlp::traces))
        .route("/otlp/v1/metrics", post(otlp::metrics))
}

/// What retention deletes: rows older than each cutoff.
pub struct Cutoffs {
    pub logs_before_ms: i64,
    pub spans_before_us: i64,
    /// Unix minutes, as `span_rollups.minute`.
    pub rollups_before_minute: i64,
}

/// Old logs, spans, and rollups, and every row of a deleted source and then the source
/// itself, in batches of 500 so no statement holds the database long.
pub async fn prune(db: &Database, cutoffs: &Cutoffs) -> Result<(), sqlx::Error> {
    let sources = sqlx::query("SELECT id, deleted_at FROM telemetry_sources")
        .fetch_all(db.pool())
        .await?;
    for row in &sources {
        let id: String = row.try_get("id")?;
        let deleted = row.try_get::<Option<i64>, _>("deleted_at")?.is_some();
        // i64::MAX takes every row of a deleted source.
        let before = |cutoff: i64| if deleted { i64::MAX } else { cutoff };
        for (sql, cutoff) in [
            (
                "DELETE FROM logs WHERE id IN \
                 (SELECT id FROM logs WHERE source_id = $1 AND time_ms < $2 LIMIT 500)",
                cutoffs.logs_before_ms,
            ),
            (
                "DELETE FROM spans WHERE (source_id, trace_id, span_id) IN \
                 (SELECT source_id, trace_id, span_id FROM spans \
                 WHERE source_id = $1 AND start_us < $2 LIMIT 500)",
                cutoffs.spans_before_us,
            ),
            (
                "DELETE FROM span_rollups WHERE (source_id, service, operation, minute) IN \
                 (SELECT source_id, service, operation, minute FROM span_rollups \
                 WHERE source_id = $1 AND minute < $2 LIMIT 500)",
                cutoffs.rollups_before_minute,
            ),
        ] {
            loop {
                let count = sqlx::query(sql)
                    .bind(&id)
                    .bind(before(cutoff))
                    .execute(db.pool())
                    .await?
                    .rows_affected();
                if count < 500 {
                    break;
                }
            }
        }
        if deleted {
            sqlx::query("DELETE FROM telemetry_sources WHERE id = $1")
                .bind(&id)
                .execute(db.pool())
                .await?;
        } else {
            // An operation with no rollup rows left frees its place under the cap of 200.
            sqlx::query(
                "DELETE FROM span_operations WHERE source_id = $1 AND NOT EXISTS \
                 (SELECT 1 FROM span_rollups r WHERE r.source_id = span_operations.source_id \
                 AND r.service = span_operations.service AND r.operation = span_operations.operation)",
            )
            .bind(&id)
            .execute(db.pool())
            .await?;
        }
    }
    Ok(())
}
