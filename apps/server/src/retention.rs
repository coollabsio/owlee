//! Deletes raw ingest rows in batches. Analytics daily rollups stay, because
//! those are what the dashboards read. An issue stays while it has a stored
//! event, and goes once its last event has aged out. Analytics visits
//! and per-day visitor ids go with the raw events, and `insight_event_data`
//! follows its event by foreign-key cascade. Dead sessions and used or expired
//! email links go too.

use owlee_platform::Database;

use crate::state::AppState;

const BATCH: i64 = 500;

pub struct PruneStats {
    pub error_events: u64,
    pub error_issues: u64,
    pub analytics_events: u64,
}

pub async fn prune_expired(state: &AppState) -> Result<PruneStats, sqlx::Error> {
    let now = chrono::Utc::now();
    let error_before = (now - chrono::Duration::days(state.config.ingest.error_retention_days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let analytics_before = now.timestamp()
        - state
            .config
            .ingest
            .analytics_retention_days
            .saturating_mul(86_400);
    prune_auth(&state.db, now.timestamp_millis()).await?;
    // The spike baseline reads 7 days. One more covers the hour in progress.
    sqlx::query("DELETE FROM error_hourly_usage WHERE hour < $1")
        .bind(now.timestamp() - 8 * 86_400)
        .execute(state.db.pool())
        .await?;
    crate::errors::prune_replay_usage(&state.db, now.timestamp() - 8 * 86_400).await?;
    let replay_before = now.timestamp_millis()
        - state
            .config
            .ingest
            .replay_retention_days
            .saturating_mul(86_400_000);
    crate::errors::prune_replays(&state.db, replay_before).await?;
    let ingest = &state.config.ingest;
    let now_ms = now.timestamp_millis();
    crate::telemetry::prune(
        &state.db,
        &crate::telemetry::Cutoffs {
            logs_before_ms: now_ms - ingest.log_retention_days.saturating_mul(86_400_000),
            spans_before_us: (now_ms - ingest.trace_retention_days.saturating_mul(86_400_000))
                .saturating_mul(1000),
            rollups_before_minute: (now_ms
                - ingest.rollup_retention_days.saturating_mul(86_400_000))
            .div_euclid(60_000),
        },
    )
    .await?;
    // Loading this month's salt deletes last month's, even with no analytics traffic.
    state
        .insights_salt
        .for_time(&state.db, now.timestamp())
        .await?;
    prune_before(&state.db, &error_before, analytics_before).await
}

async fn prune_auth(db: &Database, now_ms: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM sessions WHERE revoked_at IS NOT NULL OR idle_expires_at <= $1 OR absolute_expires_at <= $1",
    )
    .bind(now_ms)
    .execute(db.pool())
    .await?;
    sqlx::query("DELETE FROM email_tokens WHERE used_at IS NOT NULL OR expires_at <= $1")
        .bind(now_ms)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn prune_before(
    db: &Database,
    error_before: &str,
    analytics_before_unix: i64,
) -> Result<PruneStats, sqlx::Error> {
    let error_events = delete_batches(
        db,
        "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE timestamp < $1 ORDER BY timestamp LIMIT 500)",
        error_before,
    )
    .await?;
    // An issue seen again inside the retention window stays, even when a
    // privacy request has removed its events.
    let error_issues = delete_batches(
        db,
        "DELETE FROM issues WHERE id IN (SELECT id FROM issues WHERE last_seen < $1 \
         AND NOT EXISTS (SELECT 1 FROM events WHERE events.issue_id = issues.id) LIMIT 500)",
        error_before,
    )
    .await?;
    let analytics_events = delete_unix(
        db,
        "DELETE FROM insight_events WHERE id IN (SELECT id FROM insight_events WHERE timestamp < $1 ORDER BY timestamp LIMIT 500)",
        analytics_before_unix,
    )
    .await?;
    for sql in [
        "DELETE FROM insight_visits WHERE id IN (SELECT id FROM insight_visits WHERE last_at < $1 LIMIT 500)",
        "DELETE FROM insight_site_sessions WHERE (site_id, day, session_id) IN \
         (SELECT site_id, day, session_id FROM insight_site_sessions WHERE day < $1 LIMIT 500)",
        "DELETE FROM insight_link_sessions WHERE (link_id, day, session_id) IN \
         (SELECT link_id, day, session_id FROM insight_link_sessions WHERE day < $1 LIMIT 500)",
    ] {
        delete_unix(db, sql, analytics_before_unix).await?;
    }
    Ok(PruneStats {
        error_events,
        error_issues,
        analytics_events,
    })
}

async fn delete_batches(db: &Database, sql: &str, before: &str) -> Result<u64, sqlx::Error> {
    let mut total = 0;
    loop {
        let deleted = sqlx::query(sql)
            .bind(before)
            .execute(db.pool())
            .await?
            .rows_affected();
        total += deleted;
        if deleted == 0 || deleted < BATCH as u64 {
            break;
        }
    }
    Ok(total)
}

async fn delete_unix(db: &Database, sql: &str, before: i64) -> Result<u64, sqlx::Error> {
    let mut total = 0;
    loop {
        let deleted = sqlx::query(sql)
            .bind(before)
            .execute(db.pool())
            .await?
            .rows_affected();
        total += deleted;
        if deleted == 0 || deleted < BATCH as u64 {
            break;
        }
    }
    Ok(total)
}
