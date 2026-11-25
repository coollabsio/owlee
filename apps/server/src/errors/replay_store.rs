//! Replay rows: the one-transaction segment store, and the reads behind the replay routes.
//! Every query takes the app id, so a replay of another app is never reached.

use chrono::{DateTime, Utc};
use owlee_errors::{RecordingEncoding, ReplaySegment};
use owlee_platform::{AppKey, Database, Id};
use serde::Serialize;
use sqlx::Row;

use super::store::hash_user_key;
use crate::error::AppError;
use crate::paging::{Cursor, Key, Paged, Paging};

/// A replay with a segment received this recently may still grow.
const LIVE_MS: i64 = 15 * 60 * 1000;
const MAX_URLS: i64 = 100;
const MAX_URL_CHARS: usize = 2048;
const MAX_LABEL_CHARS: usize = 200;
/// Client clocks are trusted from 2 hours before to 5 minutes after the receive time.
const CLAMP_BEFORE_MS: i64 = 2 * 3600 * 1000;
const CLAMP_AFTER_MS: i64 = 5 * 60 * 1000;

pub enum Stored {
    Added,
    /// The segment was stored before. No row or counter changed.
    Duplicate,
    HourlyLimit,
    DailyBytes,
}

/// The per-app caps the store checks.
pub struct Caps {
    pub hourly_limit: i64,
    pub daily_bytes: i64,
}

pub fn clamp_ms(client_ms: Option<i64>, received_ms: i64) -> i64 {
    client_ms
        .unwrap_or(received_ms)
        .clamp(received_ms - CLAMP_BEFORE_MS, received_ms + CLAMP_AFTER_MS)
}

/// One segment is one transaction: replay row, segment, byte counter, then the
/// merge. Each counter changes only when its insert added a row, so retries
/// and segments out of order converge to the same rows and counters.
pub async fn store_segment(
    db: &Database,
    app_key: &AppKey,
    app_id: &str,
    caps: &Caps,
    segment: &ReplaySegment,
    received_ms: i64,
) -> Result<Stored, AppError> {
    let time_ms = clamp_ms(segment.timestamp_ms, received_ms);
    let bytes = segment.data.len() as i64;
    let mut tx = db.begin_immediate().await?;
    let added_replay = sqlx::query(
        "INSERT INTO replays (app_id, replay_id, replay_type, has_start, started_ms, last_ms, \
         received_ms, last_received_ms, segment_count, bytes, url_count) \
         VALUES ($1, $2, $3, 0, NULL, $4, $5, $5, 0, 0, 0) ON CONFLICT DO NOTHING",
    )
    .bind(app_id)
    .bind(&segment.replay_id)
    .bind(segment.replay_type.as_str())
    .bind(time_ms)
    .bind(received_ms)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if added_replay {
        let reserved = sqlx::query(
            "INSERT INTO replay_hourly_usage (app_id, hour, replays) VALUES ($1, $2, 1) \
             ON CONFLICT (app_id, hour) DO UPDATE SET replays = replay_hourly_usage.replays + 1 \
             WHERE replay_hourly_usage.replays < $3 RETURNING replays",
        )
        .bind(app_id)
        .bind(hour_start(received_ms))
        .bind(caps.hourly_limit)
        .fetch_optional(&mut *tx)
        .await?;
        if reserved.is_none() {
            tx.rollback().await?;
            return Ok(Stored::HourlyLimit);
        }
    }
    let added_segment = sqlx::query(
        "INSERT INTO replay_segments (app_id, replay_id, segment_id, time_ms, encoding, data) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
    )
    .bind(app_id)
    .bind(&segment.replay_id)
    .bind(i64::from(segment.segment_id))
    .bind(time_ms)
    .bind(segment.encoding.as_str())
    .bind(segment.data.clone())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !added_segment {
        tx.commit().await?;
        return Ok(Stored::Duplicate);
    }
    let reserved = if bytes > caps.daily_bytes {
        None
    } else {
        sqlx::query(
            "INSERT INTO replay_daily_usage (app_id, day, bytes) VALUES ($1, $2, $3) \
             ON CONFLICT (app_id, day) DO UPDATE SET bytes = replay_daily_usage.bytes + excluded.bytes \
             WHERE replay_daily_usage.bytes + excluded.bytes <= $4 RETURNING bytes",
        )
        .bind(app_id)
        .bind(day_start(received_ms))
        .bind(bytes)
        .bind(caps.daily_bytes)
        .fetch_optional(&mut *tx)
        .await?
    };
    if reserved.is_none() {
        tx.rollback().await?;
        return Ok(Stored::DailyBytes);
    }
    let urls = clean_urls(&segment.urls);
    for url in &urls {
        sqlx::query(
            "INSERT INTO replay_urls (app_id, replay_id, url) SELECT $1, $2, $3 \
             WHERE (SELECT COUNT(*) FROM replay_urls WHERE app_id = $1 AND replay_id = $2) < $4 \
             ON CONFLICT DO NOTHING",
        )
        .bind(app_id)
        .bind(&segment.replay_id)
        .bind(url)
        .bind(MAX_URLS)
        .execute(&mut *tx)
        .await?;
    }
    for hash in user_hashes(app_key, segment) {
        sqlx::query(
            "INSERT INTO replay_users (user_hash, app_id, replay_id) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING",
        )
        .bind(hash)
        .bind(app_id)
        .bind(&segment.replay_id)
        .execute(&mut *tx)
        .await?;
    }
    let is_start = segment.segment_id == 0;
    let started_ms =
        is_start.then(|| clamp_ms(segment.start_ms.or(segment.timestamp_ms), received_ms));
    let (browser, os, _) = segment
        .user_agent
        .as_deref()
        .map(owlee_insights::parse_user_agent)
        .unwrap_or_default();
    sqlx::query(
        "UPDATE replays SET \
            segment_count = segment_count + 1, \
            bytes = bytes + $3, \
            last_ms = CASE WHEN last_ms < $4 THEN $4 ELSE last_ms END, \
            last_received_ms = CASE WHEN last_received_ms < $5 THEN $5 ELSE last_received_ms END, \
            has_start = CASE WHEN $6 = 1 THEN 1 ELSE has_start END, \
            started_ms = CASE WHEN $6 = 1 THEN $7 ELSE started_ms END, \
            replay_type = CASE WHEN $8 = 'session' THEN 'session' ELSE replay_type END, \
            first_url = COALESCE(first_url, $9), \
            release = COALESCE(release, $10), \
            environment = COALESCE(environment, $11), \
            browser = COALESCE(browser, $12), \
            os = COALESCE(os, $13), \
            user_label = COALESCE(user_label, $14), \
            url_count = (SELECT COUNT(*) FROM replay_urls u WHERE u.app_id = $1 AND u.replay_id = $2) \
         WHERE app_id = $1 AND replay_id = $2",
    )
    .bind(app_id)
    .bind(&segment.replay_id)
    .bind(bytes)
    .bind(time_ms)
    .bind(received_ms)
    .bind(i64::from(is_start))
    .bind(started_ms)
    .bind(segment.replay_type.as_str())
    .bind(urls.first().cloned())
    .bind(segment.release.as_deref().map(label))
    .bind(segment.environment.as_deref().map(label))
    .bind(browser)
    .bind(os)
    .bind(segment.user_label().as_deref().map(label))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Stored::Added)
}

fn hour_start(ms: i64) -> i64 {
    let seconds = ms.div_euclid(1000);
    seconds - seconds.rem_euclid(3600)
}

fn day_start(ms: i64) -> i64 {
    let seconds = ms.div_euclid(1000);
    seconds - seconds.rem_euclid(86_400)
}

fn label(text: &str) -> String {
    text.chars().take(MAX_LABEL_CHARS).collect()
}

/// Sensitive query values become `[Filtered]`, as in error events.
fn clean_urls(urls: &[String]) -> Vec<String> {
    let mut cleaned: Vec<String> = Vec::new();
    for url in urls.iter().take(MAX_URLS as usize) {
        let url: String = url.chars().take(MAX_URL_CHARS).collect();
        let url = match url.split_once('?') {
            Some((base, query)) => format!("{base}?{}", owlee_errors::scrub_query(query)),
            None => url,
        };
        let url = owlee_errors::scrub_text(&url).into_owned();
        if !cleaned.contains(&url) {
            cleaned.push(url);
        }
    }
    cleaned
}

/// The same keyed hash as `event_users`. The IP address is never kept.
fn user_hashes(app_key: &AppKey, segment: &ReplaySegment) -> Vec<String> {
    let mut hashes = Vec::new();
    for key in ["id", "email", "username"] {
        let value = segment
            .event
            .get("user")
            .and_then(|user| user.get(key))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "{{auto}}");
        if let Some(value) = value {
            let hash = hash_user_key(app_key, value);
            if !hashes.contains(&hash) {
                hashes.push(hash);
            }
        }
    }
    hashes
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySummary {
    pub id: String,
    pub replay_type: String,
    pub has_start: bool,
    pub started_ms: Option<i64>,
    pub last_ms: i64,
    pub received_ms: i64,
    pub last_received_ms: i64,
    pub duration_ms: Option<i64>,
    pub segment_count: i64,
    pub bytes: i64,
    pub first_url: Option<String>,
    pub url_count: i64,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub browser: Option<String>,
    pub os: Option<String>,
    pub user_label: Option<String>,
    pub error_count: i64,
    pub live: bool,
}

const SUMMARY_COLUMNS: &str = "r.replay_id, r.replay_type, r.has_start, r.started_ms, r.last_ms, \
     r.received_ms, r.last_received_ms, r.segment_count, r.bytes, r.first_url, r.url_count, \
     r.release, r.environment, r.browser, r.os, r.user_label, \
     (SELECT CAST(COUNT(*) AS BIGINT) FROM events e WHERE e.app_id = r.app_id AND e.replay_id = r.replay_id) AS error_count";

fn summary(row: &sqlx::any::AnyRow, now_ms: i64) -> Result<ReplaySummary, AppError> {
    let has_start = row.try_get::<i64, _>("has_start")? != 0;
    let started_ms: Option<i64> = row.try_get("started_ms")?;
    let last_ms: i64 = row.try_get("last_ms")?;
    let last_received_ms: i64 = row.try_get("last_received_ms")?;
    Ok(ReplaySummary {
        id: row.try_get("replay_id")?,
        replay_type: row.try_get("replay_type")?,
        has_start,
        started_ms,
        last_ms,
        received_ms: row.try_get("received_ms")?,
        last_received_ms,
        duration_ms: started_ms
            .filter(|_| has_start)
            .map(|started| (last_ms - started).max(0)),
        segment_count: row.try_get("segment_count")?,
        bytes: row.try_get("bytes")?,
        first_url: row.try_get("first_url")?,
        url_count: row.try_get("url_count")?,
        release: row.try_get("release")?,
        environment: row.try_get("environment")?,
        browser: row.try_get("browser")?,
        os: row.try_get("os")?,
        user_label: row.try_get("user_label")?,
        error_count: row.try_get("error_count")?,
        live: now_ms - last_received_ms <= LIVE_MS,
    })
}

#[derive(Default)]
pub struct ReplayFilter {
    pub url: String,
    pub release: String,
    pub environment: String,
    pub has_errors: bool,
}

/// Newest first by `(received_ms, replay_id)`.
pub async fn list(
    db: &Database,
    app_id: &str,
    filter: &ReplayFilter,
    paging: &Paging,
) -> Result<Paged<ReplaySummary>, AppError> {
    let after = paging.int_after()?;
    let (keyset, limit) = if after.is_some() {
        (
            "AND (r.received_ms < $9 OR (r.received_ms = $9 AND r.replay_id < $10))",
            "$11",
        )
    } else {
        ("", "$9")
    };
    let url = filter.url.trim();
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM replays r
         WHERE r.app_id = $1
           AND ($2 = '' OR EXISTS (SELECT 1 FROM replay_urls u WHERE u.app_id = r.app_id
                AND u.replay_id = r.replay_id AND lower(u.url) LIKE $3 ESCAPE '\\'))
           AND ($4 = '' OR r.release = $5)
           AND ($6 = '' OR r.environment = $7)
           AND ($8 = 0 OR EXISTS (SELECT 1 FROM events e WHERE e.app_id = r.app_id AND e.replay_id = r.replay_id))
           {keyset}
         ORDER BY r.received_ms DESC, r.replay_id DESC LIMIT {limit}"
    );
    let mut query = sqlx::query(&sql)
        .bind(app_id)
        .bind(url)
        .bind(like_pattern(url))
        .bind(&filter.release)
        .bind(&filter.release)
        .bind(&filter.environment)
        .bind(&filter.environment)
        .bind(i64::from(filter.has_errors));
    if let Some((after_ms, after_id)) = after {
        query = query.bind(after_ms).bind(after_id);
    }
    let rows = query.bind(paging.fetch()).fetch_all(db.pool()).await?;
    let now = Utc::now().timestamp_millis();
    let items = rows
        .iter()
        .map(|row| summary(row, now))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(paging.finish(items, |item| {
        Cursor::new(Key::Int(item.received_ms), item.id.clone())
    }))
}

fn like_pattern(search: &str) -> String {
    let mut escaped = String::new();
    for ch in search.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            escaped.push('\\');
        }
        escaped.extend(ch.to_lowercase());
    }
    format!("%{escaped}%")
}

pub async fn summary_of(
    db: &Database,
    app_id: &str,
    replay_id: &str,
) -> Result<ReplaySummary, AppError> {
    let row = sqlx::query(&format!(
        "SELECT {SUMMARY_COLUMNS} FROM replays r WHERE r.app_id = $1 AND r.replay_id = $2"
    ))
    .bind(app_id)
    .bind(replay_id)
    .fetch_optional(db.pool())
    .await?
    .ok_or(AppError::NotFound)?;
    summary(&row, Utc::now().timestamp_millis())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayError {
    pub event_id: String,
    pub issue_id: String,
    pub title: String,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayDetail {
    #[serde(flatten)]
    pub summary: ReplaySummary,
    pub urls: Vec<String>,
    pub segments: Vec<i64>,
    pub errors: Vec<ReplayError>,
}

/// The linked events a replay page lists.
const MAX_ERRORS: i64 = 500;

pub async fn detail(
    db: &Database,
    app_id: &str,
    replay_id: &str,
) -> Result<ReplayDetail, AppError> {
    let summary = summary_of(db, app_id, replay_id).await?;
    let urls = sqlx::query(
        "SELECT url FROM replay_urls WHERE app_id = $1 AND replay_id = $2 ORDER BY url",
    )
    .bind(app_id)
    .bind(replay_id)
    .fetch_all(db.pool())
    .await?
    .iter()
    .map(|row| row.try_get("url"))
    .collect::<Result<_, _>>()?;
    let segments = sqlx::query(
        "SELECT segment_id FROM replay_segments WHERE app_id = $1 AND replay_id = $2 ORDER BY segment_id",
    )
    .bind(app_id)
    .bind(replay_id)
    .fetch_all(db.pool())
    .await?
    .iter()
    .map(|row| row.try_get("segment_id"))
    .collect::<Result<_, _>>()?;
    let rows = sqlx::query(
        "SELECT id, issue_id, title, timestamp FROM events WHERE app_id = $1 AND replay_id = $2 \
         ORDER BY timestamp, id LIMIT $3",
    )
    .bind(app_id)
    .bind(replay_id)
    .bind(MAX_ERRORS)
    .fetch_all(db.pool())
    .await?;
    let mut errors = Vec::with_capacity(rows.len());
    for row in &rows {
        let timestamp: String = row.try_get("timestamp")?;
        errors.push(ReplayError {
            event_id: row.try_get("id")?,
            issue_id: row.try_get("issue_id")?,
            title: row.try_get("title")?,
            timestamp_ms: DateTime::parse_from_rfc3339(&timestamp)
                .map(|time| time.with_timezone(&Utc).timestamp_millis())
                .unwrap_or(0),
        });
    }
    Ok(ReplayDetail {
        summary,
        urls,
        segments,
        errors,
    })
}

pub async fn segment(
    db: &Database,
    app_id: &str,
    replay_id: &str,
    segment_id: i64,
) -> Result<(RecordingEncoding, Vec<u8>), AppError> {
    let row = sqlx::query(
        "SELECT encoding, data FROM replay_segments WHERE app_id = $1 AND replay_id = $2 AND segment_id = $3",
    )
    .bind(app_id)
    .bind(replay_id)
    .bind(segment_id)
    .fetch_optional(db.pool())
    .await?
    .ok_or(AppError::NotFound)?;
    let encoding = match row.try_get::<String, _>("encoding")?.as_str() {
        "deflate" => RecordingEncoding::Deflate,
        _ => RecordingEncoding::Identity,
    };
    Ok((encoding, row.try_get("data")?))
}

/// Segments, URLs, and user hashes go by cascade.
pub async fn delete(db: &Database, app_id: &str, replay_id: &str) -> Result<(), AppError> {
    let deleted = sqlx::query("DELETE FROM replays WHERE app_id = $1 AND replay_id = $2")
        .bind(app_id)
        .bind(replay_id)
        .execute(db.pool())
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

pub async fn set_settings(
    db: &Database,
    app_id: &str,
    enabled: bool,
    daily_bytes: i64,
    hourly_limit: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE error_apps SET replays_enabled = $1, replay_daily_bytes = $2, replay_hourly_limit = $3 WHERE id = $4",
    )
    .bind(i64::from(enabled))
    .bind(daily_bytes)
    .bind(hourly_limit)
    .bind(app_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub struct Usage {
    pub today_bytes: i64,
    pub hour_replays: i64,
    pub total_bytes: i64,
}

pub async fn usage(db: &Database, app_id: &str) -> Result<Usage, AppError> {
    let now = Utc::now().timestamp_millis();
    let row = sqlx::query(
        "SELECT \
            (SELECT bytes FROM replay_daily_usage WHERE app_id = $1 AND day = $2) AS today_bytes, \
            (SELECT replays FROM replay_hourly_usage WHERE app_id = $1 AND hour = $3) AS hour_replays, \
            (SELECT CAST(SUM(bytes) AS BIGINT) FROM replays WHERE app_id = $1) AS total_bytes",
    )
    .bind(app_id)
    .bind(day_start(now))
    .bind(hour_start(now))
    .fetch_one(db.pool())
    .await?;
    Ok(Usage {
        today_bytes: row.try_get::<Option<i64>, _>("today_bytes")?.unwrap_or(0),
        hour_replays: row.try_get::<Option<i64>, _>("hour_replays")?.unwrap_or(0),
        total_bytes: row.try_get::<Option<i64>, _>("total_bytes")?.unwrap_or(0),
    })
}

/// The replay an event links to, when it exists in the event's app.
pub async fn started_ms_of(
    db: &Database,
    app_id: &str,
    replay_id: &str,
) -> Result<Option<Option<i64>>, AppError> {
    let row = sqlx::query("SELECT started_ms FROM replays WHERE app_id = $1 AND replay_id = $2")
        .bind(app_id)
        .bind(replay_id)
        .fetch_optional(db.pool())
        .await?;
    Ok(row.map(|row| row.try_get("started_ms")).transpose()?)
}

/// Events of the issue that link to a replay stored in the same app.
pub async fn issue_replay_event_count(db: &Database, issue_id: &str) -> Result<i64, AppError> {
    let row = sqlx::query(
        "SELECT CAST(COUNT(*) AS BIGINT) AS count FROM events e WHERE e.issue_id = $1 \
         AND e.replay_id IS NOT NULL \
         AND EXISTS (SELECT 1 FROM replays r WHERE r.app_id = e.app_id AND r.replay_id = e.replay_id)",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await?;
    Ok(row.try_get("count")?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserReplay {
    pub app_id: String,
    pub app_name: String,
    #[serde(flatten)]
    pub summary: ReplaySummary,
    pub urls: Vec<String>,
    /// Paths of the segment route, one per stored segment.
    pub segments: Vec<String>,
}

const USER_REPLAYS: &str = "(r.app_id, r.replay_id) IN (SELECT app_id, replay_id FROM replay_users WHERE user_hash = $1) \
     AND r.app_id IN (SELECT a.id FROM error_apps a JOIN projects pr ON pr.id = a.project_id WHERE pr.workspace_id = $2)";

/// Only replays of the workspace's apps. The same person in another workspace stays untouched.
pub async fn replays_for_user(
    db: &Database,
    workspace_id: Id,
    user_hash: &str,
) -> Result<Vec<UserReplay>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT r.app_id, a.name AS app_name, {SUMMARY_COLUMNS} FROM replays r \
         JOIN error_apps a ON a.id = r.app_id WHERE {USER_REPLAYS} ORDER BY r.received_ms"
    ))
    .bind(user_hash)
    .bind(workspace_id.to_string())
    .fetch_all(db.pool())
    .await?;
    let now = Utc::now().timestamp_millis();
    let mut replays = Vec::with_capacity(rows.len());
    for row in &rows {
        let app_id: String = row.try_get("app_id")?;
        let summary = summary(row, now)?;
        let detail = detail(db, &app_id, &summary.id).await?;
        replays.push(UserReplay {
            segments: detail
                .segments
                .iter()
                .map(|segment| {
                    format!(
                        "/api/error-apps/{app_id}/replays/{}/segments/{segment}",
                        summary.id
                    )
                })
                .collect(),
            urls: detail.urls,
            app_name: row.try_get("app_name")?,
            app_id,
            summary,
        });
    }
    Ok(replays)
}

pub async fn delete_replays_for_user(
    db: &Database,
    workspace_id: Id,
    user_hash: &str,
) -> Result<u64, AppError> {
    Ok(sqlx::query(&format!(
        "DELETE FROM replays WHERE (app_id, replay_id) IN \
         (SELECT r.app_id, r.replay_id FROM replays r WHERE {USER_REPLAYS})"
    ))
    .bind(user_hash)
    .bind(workspace_id.to_string())
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// Deletes replays whose last segment arrived before `before_ms`, by the
/// server clock, so a future client timestamp cannot keep one. Segments go
/// first in batches of 500, then the replay rows, whose URLs and user hashes
/// follow by cascade. Returns the number of replays deleted.
pub async fn prune_replays(db: &Database, before_ms: i64) -> Result<u64, sqlx::Error> {
    delete_batches(
        db,
        "DELETE FROM replay_segments WHERE (app_id, replay_id, segment_id) IN \
         (SELECT s.app_id, s.replay_id, s.segment_id FROM replay_segments s \
          JOIN replays r ON r.app_id = s.app_id AND r.replay_id = s.replay_id \
          WHERE r.last_received_ms < $1 LIMIT 500)",
        before_ms,
    )
    .await?;
    delete_batches(
        db,
        "DELETE FROM replays WHERE (app_id, replay_id) IN \
         (SELECT app_id, replay_id FROM replays WHERE last_received_ms < $1 LIMIT 500)",
        before_ms,
    )
    .await
}

/// Usage rows of days and hours that no cap reads anymore.
pub async fn prune_usage(db: &Database, before_unix: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM replay_daily_usage WHERE day < $1")
        .bind(before_unix)
        .execute(db.pool())
        .await?;
    sqlx::query("DELETE FROM replay_hourly_usage WHERE hour < $1")
        .bind(before_unix)
        .execute(db.pool())
        .await?;
    Ok(())
}

async fn delete_batches(db: &Database, sql: &str, before: i64) -> Result<u64, sqlx::Error> {
    let mut total = 0;
    loop {
        let deleted = sqlx::query(sql)
            .bind(before)
            .execute(db.pool())
            .await?
            .rows_affected();
        total += deleted;
        if deleted < 500 {
            return Ok(total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_times_are_clamped_to_the_receive_window() {
        let received = 1_800_000_000_000;
        assert_eq!(
            clamp_ms(Some(4_102_444_800_000), received),
            received + CLAMP_AFTER_MS
        );
        assert_eq!(clamp_ms(Some(0), received), received - CLAMP_BEFORE_MS);
        assert_eq!(clamp_ms(Some(received - 1000), received), received - 1000);
        assert_eq!(clamp_ms(None, received), received);
    }

    #[test]
    fn urls_are_scrubbed_deduplicated_and_capped() {
        let mut urls = vec![
            "https://shop.example/cart?token=abc&page=2".to_owned(),
            "https://shop.example/cart?token=xyz&page=2".to_owned(),
        ];
        urls.extend((0..200).map(|index| format!("https://shop.example/{index}")));
        let cleaned = clean_urls(&urls);
        assert_eq!(
            cleaned[0],
            "https://shop.example/cart?token=[Filtered]&page=2"
        );
        assert_eq!(cleaned[1], "https://shop.example/0");
        assert_eq!(cleaned.len(), 99);
    }
}
