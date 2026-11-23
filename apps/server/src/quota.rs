//! Per-app and per-site daily ingest quotas.
//!
//! The instance number is the default for every error app and site. A subject
//! with its own `daily_quota` uses that instead. The counter is still per
//! subject, so one app cannot spend another's allowance.

use chrono::{Duration, Utc};
use owlee_platform::{Database, Id};
use sqlx::Row;

use crate::error::AppError;

pub const ERROR: &str = "error";
pub const ANALYTICS: &str = "analytics";

pub struct UsageHit {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub used: i64,
    pub quota: i64,
}

/// `custom` replaces `global` when the app or site set its own cap.
pub fn effective(global: i64, custom: Option<i64>) -> i64 {
    custom.unwrap_or(global)
}

pub async fn error_quota(db: &Database) -> Result<i64, AppError> {
    let (error, _) = limits(db).await?;
    Ok(error)
}

pub async fn analytics_quota(db: &Database) -> Result<i64, AppError> {
    let (_, analytics) = limits(db).await?;
    Ok(analytics)
}

pub async fn limits(db: &Database) -> Result<(i64, i64), AppError> {
    let row = sqlx::query(
        "SELECT error_daily_quota, analytics_daily_quota FROM instance_settings WHERE id = 1",
    )
    .fetch_one(db.pool())
    .await?;
    Ok((
        row.try_get::<i64, _>("error_daily_quota")?,
        row.try_get::<i64, _>("analytics_daily_quota")?,
    ))
}

pub async fn used(db: &Database, kind: &str, subject_id: &str) -> Result<i64, AppError> {
    let row = sqlx::query(
        "SELECT count FROM ingest_usage WHERE kind = $1 AND subject_id = $2 AND day = $3",
    )
    .bind(kind)
    .bind(subject_id)
    .bind(today())
    .fetch_optional(db.pool())
    .await?;
    Ok(row
        .map(|row| row.try_get::<i64, _>("count"))
        .transpose()?
        .unwrap_or(0))
}

/// Adds one event to today's counter unless it is already at `quota`.
/// One statement, so parallel requests cannot pass the cap together.
/// Returns false when the cap is reached.
pub async fn reserve<'e, E>(
    executor: E,
    kind: &str,
    subject_id: &str,
    quota: i64,
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let row = sqlx::query(
        "INSERT INTO ingest_usage (kind, subject_id, day, count) VALUES ($1, $2, $3, 1) \
         ON CONFLICT (kind, subject_id, day) DO UPDATE SET count = ingest_usage.count + 1 \
         WHERE ingest_usage.count < $4 \
         RETURNING count",
    )
    .bind(kind)
    .bind(subject_id)
    .bind(today())
    .bind(quota)
    .fetch_optional(executor)
    .await?;
    Ok(row.is_some())
}

/// Hours of history in the spike baseline.
const BASELINE_HOURS: i64 = 168;

/// The most events an app may store in one UTC hour, from the counts of the
/// 168 hours before it: `max(ceil(cap / 24), ceil(3 * mean + 3 * sd))`.
pub fn spike_limit(sum: i64, sum_sq: i64, daily_cap: i64) -> i64 {
    let hours = BASELINE_HOURS as f64;
    let mean = sum as f64 / hours;
    let sd = (sum_sq as f64 / hours - mean * mean).max(0.0).sqrt();
    let floor = (daily_cap.max(1) + 23) / 24;
    floor.max((3.0 * mean + 3.0 * sd).ceil() as i64)
}

/// Unix second of the current UTC hour's start.
pub fn current_hour() -> i64 {
    let now = Utc::now().timestamp();
    now - now.rem_euclid(3600)
}

pub fn secs_until_next_hour() -> u64 {
    let now = Utc::now().timestamp();
    u64::try_from(3600 - now.rem_euclid(3600)).unwrap_or(1)
}

pub async fn hour_used<'e, E>(executor: E, app_id: &str, hour: i64) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let row = sqlx::query("SELECT count FROM error_hourly_usage WHERE app_id = $1 AND hour = $2")
        .bind(app_id)
        .bind(hour)
        .fetch_optional(executor)
        .await?;
    row.map(|row| row.try_get::<i64, _>("count"))
        .transpose()
        .map(|count| count.unwrap_or(0))
}

/// `sum(count)` and `sum(count * count)` over the baseline hours before `hour`.
pub async fn hour_baseline<'e, E>(
    executor: E,
    app_id: &str,
    hour: i64,
) -> Result<(i64, i64), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let row = sqlx::query(
        "SELECT CAST(COALESCE(SUM(count), 0) AS BIGINT) AS total, \
                CAST(COALESCE(SUM(count * count), 0) AS BIGINT) AS squares \
         FROM error_hourly_usage WHERE app_id = $1 AND hour >= $2 AND hour < $3",
    )
    .bind(app_id)
    .bind(hour - BASELINE_HOURS * 3600)
    .bind(hour)
    .fetch_one(executor)
    .await?;
    Ok((row.try_get("total")?, row.try_get("squares")?))
}

/// The hourly twin of `reserve`. Returns false when the hour is at `limit`.
pub async fn reserve_hour<'e, E>(
    executor: E,
    app_id: &str,
    hour: i64,
    limit: i64,
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let row = sqlx::query(
        "INSERT INTO error_hourly_usage (app_id, hour, count) VALUES ($1, $2, 1) \
         ON CONFLICT (app_id, hour) DO UPDATE SET count = error_hourly_usage.count + 1 \
         WHERE error_hourly_usage.count < $3 \
         RETURNING count",
    )
    .bind(app_id)
    .bind(hour)
    .bind(limit)
    .fetch_optional(executor)
    .await?;
    Ok(row.is_some())
}

/// Apps and sites of the workspace, or of one project in it, that spent today's cap.
pub async fn hits(
    db: &Database,
    workspace_id: Id,
    project_id: Option<Id>,
) -> Result<Vec<UsageHit>, AppError> {
    let day = today();
    let workspace = workspace_id.to_string();
    let project = project_id.map(|id| id.to_string()).unwrap_or_default();
    let mut hits = Vec::new();
    let errors = sqlx::query(
        "SELECT p.id, p.name, u.count AS used, \
                COALESCE(p.daily_quota, s.error_daily_quota) AS quota \
         FROM error_apps p \
         JOIN projects pr ON pr.id = p.project_id \
         JOIN instance_settings s ON s.id = 1 \
         JOIN ingest_usage u ON u.kind = 'error' AND u.subject_id = p.id AND u.day = $1 \
         WHERE u.count >= COALESCE(p.daily_quota, s.error_daily_quota) \
           AND pr.workspace_id = $2 AND ($3 = '' OR pr.id = $3) \
         ORDER BY p.name",
    )
    .bind(&day)
    .bind(&workspace)
    .bind(&project)
    .fetch_all(db.pool())
    .await?;
    for row in &errors {
        hits.push(UsageHit {
            kind: ERROR,
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            used: row.try_get("used")?,
            quota: row.try_get("quota")?,
        });
    }
    let sites = sqlx::query(
        "SELECT p.id, p.name, u.count AS used, \
                COALESCE(p.daily_quota, s.analytics_daily_quota) AS quota \
         FROM insight_sites p \
         JOIN projects pr ON pr.id = p.project_id \
         JOIN instance_settings s ON s.id = 1 \
         JOIN ingest_usage u ON u.kind = 'analytics' AND u.subject_id = p.id AND u.day = $1 \
         WHERE u.count >= COALESCE(p.daily_quota, s.analytics_daily_quota) \
           AND pr.workspace_id = $2 AND ($3 = '' OR pr.id = $3) \
         ORDER BY p.name",
    )
    .bind(&day)
    .bind(&workspace)
    .bind(&project)
    .fetch_all(db.pool())
    .await?;
    for row in &sites {
        hits.push(UsageHit {
            kind: ANALYTICS,
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            used: row.try_get("used")?,
            quota: row.try_get("quota")?,
        });
    }
    Ok(hits)
}

pub fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

/// Next UTC midnight, when today's counters are discarded.
pub fn resets_at() -> String {
    let now = Utc::now();
    let next = (now.date_naive() + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap_or(now.naive_utc());
    next.and_utc()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn secs_until_utc_midnight() -> u64 {
    let now = Utc::now();
    let next = (now.date_naive() + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap_or(now.naive_utc());
    let seconds = (next - now.naive_utc()).num_seconds();
    u64::try_from(seconds.max(1)).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::spike_limit;

    #[test]
    fn spike_limit_uses_the_floor_for_quiet_apps() {
        assert_eq!(spike_limit(0, 0, 48), 2);
        assert_eq!(spike_limit(0, 0, 100_000), 4167);
        assert_eq!(spike_limit(0, 0, 1), 1);
        assert_eq!(spike_limit(168, 168, 24), 3);
    }

    #[test]
    fn spike_limit_follows_a_busy_baseline() {
        // 168 hours of 1000: mean 1000, sd 0.
        assert_eq!(spike_limit(168_000, 168_000_000, 24_000), 3000);
        // 84 hours of 2000 and 84 of 0: mean 1000, sd 1000.
        assert_eq!(spike_limit(168_000, 336_000_000, 24_000), 6000);
        // One hour of 168 in a quiet week: mean 1, sd sqrt(167).
        assert_eq!(spike_limit(168, 168 * 168, 24), 42);
    }
}
