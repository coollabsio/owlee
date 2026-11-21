use chrono::{DateTime, Duration, SecondsFormat, Utc};
use owlee_errors::NormalizedEvent;
use owlee_platform::{AppKey, Backend, Database, Id};
use rand::RngCore;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct App {
    pub id: String,
    pub project_id: Id,
    pub name: String,
    pub slug: String,
    pub public_key: String,
    pub platform: String,
    pub created_at: String,
    /// `None` means this app uses the instance default.
    pub daily_quota: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRow {
    pub id: String,
    pub app_id: String,
    pub app_name: String,
    pub app_slug: String,
    pub title: String,
    pub culprit: Option<String>,
    pub status: String,
    pub level: String,
    pub platform: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub event_count: i64,
    pub short_id: String,
    pub unhandled: bool,
    pub priority: String,
    pub user_count: i64,
    pub period_event_count: i64,
    pub trend24h: Vec<i64>,
    pub trend14d: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSummary {
    pub id: String,
    pub timestamp: String,
    pub level: String,
    pub title: String,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub user: Option<String>,
}

fn statement(db: &Database, sql: &str) -> String {
    // sqlx Any uses `$1` on both SQLite and Postgres.
    let _ = db.backend();
    let mut rewritten = String::with_capacity(sql.len());
    let mut index = 1;
    let mut quoted = false;
    for character in sql.chars() {
        if character == '\'' {
            quoted = !quoted;
            rewritten.push(character);
        } else if character == '?' && !quoted {
            rewritten.push('$');
            rewritten.push_str(&index.to_string());
            index += 1;
        } else {
            rewritten.push(character);
        }
    }
    let _ = Backend::Sqlite;
    rewritten
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

const APP_COLUMNS: &str =
    "a.id, a.project_id, a.name, a.slug, a.public_key, a.platform, a.created_at, a.daily_quota";

/// The apps of a workspace, optionally of one project in it.
pub async fn list_apps(
    db: &Database,
    workspace_id: Id,
    project_id: Option<Id>,
) -> Result<Vec<App>, AppError> {
    let project = project_id.map(|id| id.to_string()).unwrap_or_default();
    let sql = statement(
        db,
        &format!(
            "SELECT {APP_COLUMNS} FROM error_apps a JOIN projects pr ON pr.id = a.project_id
             WHERE pr.workspace_id = ? AND (? = '' OR a.project_id = ?)
             ORDER BY a.created_at ASC"
        ),
    );
    let rows = sqlx::query(&sql)
        .bind(workspace_id.to_string())
        .bind(&project)
        .bind(&project)
        .fetch_all(db.pool())
        .await?;
    rows.iter().map(app_from_row).collect()
}

/// 404 unless the app belongs to the workspace.
pub async fn get_app(db: &Database, workspace_id: Id, id: &str) -> Result<App, AppError> {
    let sql = statement(
        db,
        &format!(
            "SELECT {APP_COLUMNS} FROM error_apps a JOIN projects pr ON pr.id = a.project_id
             WHERE a.id = ? AND pr.workspace_id = ?"
        ),
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(workspace_id.to_string())
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map_or(Err(AppError::NotFound), app_from_row)
}

/// Counts come from `issues`, not `events`, so they stay all-time after
/// retention prunes old event rows.
#[derive(Debug, Clone)]
pub struct AppActivity {
    pub issue_count: i64,
    pub event_count: i64,
    pub last_event_at: Option<String>,
}

pub async fn app_activity(db: &Database, id: &str) -> Result<AppActivity, AppError> {
    let sql = statement(
        db,
        "SELECT CAST(COUNT(*) AS BIGINT) AS issue_count,
                CAST(COALESCE(SUM(event_count), 0) AS BIGINT) AS event_count,
                MAX(last_seen) AS last_event_at
         FROM issues WHERE app_id = ?",
    );
    let row = sqlx::query(&sql).bind(id).fetch_one(db.pool()).await?;
    Ok(AppActivity {
        issue_count: row.try_get("issue_count")?,
        event_count: row.try_get("event_count")?,
        last_event_at: row.try_get("last_event_at")?,
    })
}

/// Ingest knows only the DSN, so it looks the app up without a workspace.
pub async fn app_for_ingest(db: &Database, id: &str, public_key: &str) -> Result<App, AppError> {
    let sql = statement(
        db,
        &format!("SELECT {APP_COLUMNS} FROM error_apps a WHERE a.id = ?"),
    );
    let row = sqlx::query(&sql).bind(id).fetch_optional(db.pool()).await?;
    let app = row.as_ref().map_or(Err(AppError::NotFound), app_from_row)?;
    if app.public_key != public_key {
        return Err(AppError::Unauthorized);
    }
    Ok(app)
}

/// The caller checks that `project_id` is in the member's workspace.
pub async fn create_app(
    db: &Database,
    project_id: Id,
    name: &str,
    platform: &str,
) -> Result<App, AppError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 120 {
        return Err(AppError::BadRequest(
            "App name must be between 1 and 120 characters".into(),
        ));
    }
    let platform = normalize_platform(platform);
    let slug = unique_slug(db, &slugify(name)).await?;
    let mut key_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    let public_key = hex::encode(key_bytes);
    let created_at = now();
    let mut tx = db.pool().begin().await?;
    let sql = statement(
        db,
        "UPDATE error_app_counters SET next_id = next_id + 1 WHERE id = 1 RETURNING next_id",
    );
    let row = sqlx::query(&sql).fetch_one(&mut *tx).await?;
    let next_id: i64 = row.try_get("next_id")?;
    let id = (next_id - 1).to_string();
    let sql = statement(
        db,
        "INSERT INTO error_apps (id, project_id, name, slug, public_key, platform, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    );
    sqlx::query(&sql)
        .bind(&id)
        .bind(project_id.to_string())
        .bind(name)
        .bind(&slug)
        .bind(&public_key)
        .bind(&platform)
        .bind(&created_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(App {
        id,
        project_id,
        name: name.to_owned(),
        slug,
        public_key,
        platform,
        created_at,
        daily_quota: None,
    })
}

pub async fn delete_app(db: &Database, workspace_id: Id, id: &str) -> Result<(), AppError> {
    let app = get_app(db, workspace_id, id).await?;
    let mut tx = db.pool().begin().await?;
    let sql = statement(db, "DELETE FROM events WHERE app_id = ?");
    sqlx::query(&sql).bind(&app.id).execute(&mut *tx).await?;
    let sql = statement(db, "DELETE FROM issues WHERE app_id = ?");
    sqlx::query(&sql).bind(&app.id).execute(&mut *tx).await?;
    let sql = statement(db, "DELETE FROM error_apps WHERE id = ?");
    sqlx::query(&sql).bind(&app.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub struct IssueQuery {
    pub workspace_id: Id,
    /// Empty means every project of the workspace.
    pub project_id: String,
    /// Empty means every app.
    pub app_id: String,
    pub status: String,
    pub search: String,
    pub environment: String,
    pub since: String,
    pub sort: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagValue {
    pub value: String,
    pub count: i64,
}

/// One tag key and its most common values across the matched events.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagGroup {
    pub key: String,
    pub values: Vec<TagValue>,
}

pub struct IssueAnalytics {
    /// Newest first, at most `EVENT_LIST_LIMIT`.
    pub events: Vec<EventSummary>,
    pub latest_payload: Option<String>,
    pub histogram: Vec<i64>,
    pub users: i64,
    pub period_events: i64,
    pub tags: Vec<TagGroup>,
}

const EVENT_LIST_LIMIT: usize = 100;
const TAG_KEY_LIMIT: usize = 10;
const TAG_VALUE_LIMIT: usize = 4;

/// The feed shows at most this many issues, after sorting.
const ISSUE_LIMIT: usize = 200;

/// Issues with their app and its project, for workspace filters.
const ISSUE_SOURCE: &str = "issues i
         JOIN error_apps a ON a.id = i.app_id
         JOIN projects pr ON pr.id = a.project_id";

/// Every feed filter except status. `issue_filter_binds` returns its binds in order.
const ISSUE_FILTERS: &str = "pr.workspace_id = ?
           AND (? = '' OR a.project_id = ?)
           AND (? = '' OR i.app_id = ?)
           AND (? = '' OR lower(i.title) LIKE ? ESCAPE '\\' OR lower(COALESCE(i.culprit, '')) LIKE ? ESCAPE '\\' OR lower(COALESCE(i.short_id, '')) LIKE ? ESCAPE '\\')
           AND (? = '' OR EXISTS (SELECT 1 FROM events ev WHERE ev.issue_id = i.id AND ev.environment = ?))
           AND (? = '' OR EXISTS (SELECT 1 FROM events ev WHERE ev.issue_id = i.id AND ev.timestamp >= ?))";

fn issue_filter_binds(query: &IssueQuery, since: &str) -> [String; 13] {
    let like = like_pattern(&query.search);
    let search_flag = if query.search.trim().is_empty() {
        ""
    } else {
        "1"
    };
    let since_flag = if since.is_empty() { "" } else { "1" };
    [
        query.workspace_id.to_string(),
        query.project_id.clone(),
        query.project_id.clone(),
        query.app_id.clone(),
        query.app_id.clone(),
        search_flag.to_owned(),
        like.clone(),
        like.clone(),
        like,
        query.environment.clone(),
        query.environment.clone(),
        since_flag.to_owned(),
        since.to_owned(),
    ]
}

#[derive(Debug, Default, Serialize)]
pub struct IssueCounts {
    pub unresolved: i64,
    pub resolved: i64,
    pub ignored: i64,
}

pub struct IssuePage {
    pub issues: Vec<IssueRow>,
    /// Counts per status under the same filters, ignoring the status filter.
    pub counts: IssueCounts,
    pub truncated: bool,
}

pub async fn list_issues(db: &Database, query: &IssueQuery) -> Result<IssuePage, AppError> {
    let since = since_bound(&query.since);
    let since_flag = if since.is_empty() { "" } else { "1" };
    let filters = issue_filter_binds(query, &since);
    let order = match query.sort.as_str() {
        "age" | "first_seen" => "i.first_seen ASC",
        "events" => "period_event_count DESC",
        "users" => "user_count DESC",
        _ => "i.last_seen DESC",
    };
    // The stats are columns so the sort runs before the limit.
    let sql = statement(
        db,
        &format!(
            "SELECT {ISSUE_COLUMNS},
                CAST(CASE WHEN ? = '' THEN i.event_count
                     ELSE (SELECT COUNT(*) FROM events ev WHERE ev.issue_id = i.id AND ev.timestamp >= ?)
                END AS BIGINT) AS period_event_count,
                CAST((SELECT COUNT(DISTINCT ev.user_key) FROM events ev
                      WHERE ev.issue_id = i.id AND ev.user_key != '' AND (? = '' OR ev.timestamp >= ?)) AS BIGINT) AS user_count
         FROM {ISSUE_SOURCE}
         WHERE (? = '' OR i.status = ?)
           AND {ISSUE_FILTERS}
         ORDER BY {order}, i.last_seen DESC, i.id
         LIMIT {}",
            ISSUE_LIMIT + 1
        ),
    );
    let mut select = sqlx::query(&sql)
        .bind(since_flag)
        .bind(&since)
        .bind(since_flag)
        .bind(&since)
        .bind(&query.status)
        .bind(&query.status);
    for value in &filters {
        select = select.bind(value);
    }
    let rows = select.fetch_all(db.pool()).await?;
    let truncated = rows.len() > ISSUE_LIMIT;
    let mut issues = rows
        .iter()
        .take(ISSUE_LIMIT)
        .map(|row| {
            let mut issue = issue_from_row(row)?;
            issue.period_event_count = row.try_get("period_event_count")?;
            issue.user_count = row.try_get("user_count")?;
            Ok(issue)
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    attach_trends(db, &mut issues).await?;

    let sql = statement(
        db,
        &format!(
            "SELECT i.status, CAST(COUNT(*) AS BIGINT) AS total
         FROM {ISSUE_SOURCE}
         WHERE {ISSUE_FILTERS}
         GROUP BY i.status"
        ),
    );
    let mut count = sqlx::query(&sql);
    for value in &filters {
        count = count.bind(value);
    }
    let mut counts = IssueCounts::default();
    for row in count.fetch_all(db.pool()).await? {
        let total: i64 = row.try_get("total")?;
        match row.try_get::<String, _>("status")?.as_str() {
            "unresolved" => counts.unresolved = total,
            "resolved" => counts.resolved = total,
            "ignored" => counts.ignored = total,
            _ => {}
        }
    }
    Ok(IssuePage {
        issues,
        counts,
        truncated,
    })
}

/// The environments of the workspace's events, narrowed like the issue feed.
pub async fn list_environments(
    db: &Database,
    workspace_id: Id,
    project_id: &str,
    app_id: &str,
) -> Result<Vec<String>, AppError> {
    let sql = statement(
        db,
        "SELECT DISTINCT e.environment FROM events e
         JOIN error_apps a ON a.id = e.app_id
         JOIN projects pr ON pr.id = a.project_id
         WHERE e.environment IS NOT NULL AND e.environment != ''
           AND pr.workspace_id = ?
           AND (? = '' OR a.project_id = ?)
           AND (? = '' OR e.app_id = ?)
         ORDER BY e.environment",
    );
    let rows = sqlx::query(&sql)
        .bind(workspace_id.to_string())
        .bind(project_id)
        .bind(project_id)
        .bind(app_id)
        .bind(app_id)
        .fetch_all(db.pool())
        .await?;
    Ok(rows
        .iter()
        .filter_map(|row| row.try_get::<String, _>("environment").ok())
        .collect())
}

const ISSUE_COLUMNS: &str = "i.id, i.app_id, a.name AS app_name, a.slug AS app_slug,
                i.title, i.culprit, i.status, i.level, i.platform, i.environment, i.release,
                i.first_seen, i.last_seen, i.event_count, i.short_id, i.unhandled";

/// Matches the id or the short id. 404 unless the issue's app is in the workspace.
pub async fn get_issue(db: &Database, workspace_id: Id, id: &str) -> Result<IssueRow, AppError> {
    let sql = statement(
        db,
        &format!(
            "SELECT {ISSUE_COLUMNS} FROM {ISSUE_SOURCE}
             WHERE (i.id = ? OR i.short_id = ?) AND pr.workspace_id = ?"
        ),
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(id)
        .bind(workspace_id.to_string())
        .fetch_optional(db.pool())
        .await?;
    match row {
        Some(row) => issue_from_row(&row),
        None => Err(AppError::NotFound),
    }
}

/// Keeps a statement to the apps of one workspace. Binds the workspace id.
const IN_WORKSPACE: &str = "app_id IN (SELECT a.id FROM error_apps a
         JOIN projects pr ON pr.id = a.project_id WHERE pr.workspace_id = ?)";

pub async fn update_issue_status(
    db: &Database,
    workspace_id: Id,
    id: &str,
    status: &str,
) -> Result<IssueRow, AppError> {
    let status = issue_status(status)?;
    let issue = get_issue(db, workspace_id, id).await?;
    let sql = statement(db, "UPDATE issues SET status = ? WHERE id = ?");
    sqlx::query(&sql)
        .bind(status)
        .bind(&issue.id)
        .execute(db.pool())
        .await?;
    get_issue(db, workspace_id, &issue.id).await
}

fn issue_status(status: &str) -> Result<&str, AppError> {
    if matches!(status, "unresolved" | "resolved" | "ignored") {
        Ok(status)
    } else {
        Err(AppError::BadRequest(
            "Status must be unresolved, resolved, or ignored".into(),
        ))
    }
}

pub async fn get_event(db: &Database, workspace_id: Id, id: &str) -> Result<String, AppError> {
    let sql = statement(
        db,
        &format!("SELECT payload FROM events WHERE id = ? AND {IN_WORKSPACE}"),
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(workspace_id.to_string())
        .fetch_optional(db.pool())
        .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound);
    };
    Ok(row.try_get("payload")?)
}

/// The oldest and newest stored events of an issue, ignoring any filter.
pub async fn edge_event_ids(
    db: &Database,
    issue_id: &str,
) -> Result<(Option<String>, Option<String>), AppError> {
    let sql = statement(
        db,
        "SELECT
            (SELECT id FROM events WHERE issue_id = ? ORDER BY timestamp ASC, id ASC LIMIT 1) AS first_id,
            (SELECT id FROM events WHERE issue_id = ? ORDER BY timestamp DESC, id DESC LIMIT 1) AS latest_id",
    );
    let row = sqlx::query(&sql)
        .bind(issue_id)
        .bind(issue_id)
        .fetch_one(db.pool())
        .await?;
    Ok((row.try_get("first_id")?, row.try_get("latest_id")?))
}

pub struct Ingested {
    pub id: String,
    /// The daily quota was already spent. Nothing new was stored.
    pub quota_exceeded: bool,
    pub retry_after_secs: u64,
}

pub async fn ingest_event(
    db: &Database,
    app_key: &AppKey,
    app: &App,
    event: &NormalizedEvent,
    quota: i64,
    keep_first: i64,
    window_secs: i64,
) -> Result<Ingested, AppError> {
    let mut tx = db.pool().begin().await?;
    // `events.id` is unique across apps. A resend from the same app is a
    // duplicate. The same id from another app is a different event, and must
    // not be swallowed by a row its sender cannot see, so it is stored under
    // an id derived from the app. That id is stable, so its resends dedup too.
    let sql = statement(db, "SELECT app_id FROM events WHERE id = ?");
    let derived = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{}:{}", app.id, event.event_id).as_bytes(),
    )
    .simple()
    .to_string();
    let mut event_id = None;
    for candidate in [event.event_id.clone(), derived] {
        let owner: Option<String> = sqlx::query(&sql)
            .bind(&candidate)
            .fetch_optional(&mut *tx)
            .await?
            .map(|row| row.try_get("app_id"))
            .transpose()?;
        match owner {
            None => {
                event_id = Some(candidate);
                break;
            }
            Some(owner) if owner == app.id => {
                tx.commit().await?;
                return Ok(Ingested {
                    id: candidate,
                    quota_exceeded: false,
                    retry_after_secs: 0,
                });
            }
            Some(_) => {}
        }
    }
    let event_id = event_id.unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    if !crate::quota::reserve(&mut *tx, crate::quota::ERROR, &app.id, quota).await? {
        tx.commit().await?;
        return Ok(Ingested {
            id: String::new(),
            quota_exceeded: true,
            retry_after_secs: crate::quota::secs_until_utc_midnight(),
        });
    }
    let issue_id = Uuid::new_v4().to_string();
    let seq_sql = statement(
        db,
        "UPDATE error_apps SET next_issue = next_issue + 1 WHERE id = ? RETURNING next_issue, slug",
    );
    let seq = sqlx::query(&seq_sql)
        .bind(&app.id)
        .fetch_one(&mut *tx)
        .await?;
    let next_issue: i64 = seq.try_get("next_issue")?;
    let slug: String = seq.try_get("slug")?;
    let short_id = format!("{}-{}", short_prefix(&slug), next_issue - 1);
    let unhandled = if event_unhandled(event) { 1 } else { 0 };
    let sql = statement(
        db,
        "INSERT INTO issues (
            id, app_id, fingerprint, title, culprit, status, level, platform,
            environment, release, first_seen, last_seen, event_count, short_id, unhandled
         ) VALUES (?, ?, ?, ?, ?, 'unresolved', ?, ?, ?, ?, ?, ?, 1, ?, ?)
         ON CONFLICT (app_id, fingerprint) DO UPDATE SET
            title = excluded.title,
            culprit = excluded.culprit,
            level = excluded.level,
            platform = excluded.platform,
            environment = excluded.environment,
            release = excluded.release,
            last_seen = excluded.last_seen,
            event_count = issues.event_count + 1,
            unhandled = CASE WHEN excluded.unhandled = 1 THEN 1 ELSE issues.unhandled END,
            status = CASE WHEN issues.status = 'resolved' THEN 'unresolved' ELSE issues.status END
         RETURNING id, stored_count, sample_window_start",
    );
    let row = sqlx::query(&sql)
        .bind(&issue_id)
        .bind(&app.id)
        .bind(&event.fingerprint)
        .bind(&event.title)
        .bind(&event.culprit)
        .bind(&event.level)
        .bind(event.platform.as_deref().unwrap_or(&app.platform))
        .bind(&event.environment)
        .bind(&event.release)
        .bind(&event.timestamp)
        .bind(&event.timestamp)
        .bind(&short_id)
        .bind(unhandled)
        .fetch_one(&mut *tx)
        .await?;
    let issue_id: String = row.try_get("id")?;
    let stored_count: i64 = row.try_get("stored_count")?;
    let window_start: Option<String> = row.try_get("sample_window_start")?;
    let keep = window_secs.max(1);
    let window_open = window_start
        .as_deref()
        .and_then(|start| DateTime::parse_from_rfc3339(start).ok())
        .is_some_and(|start| {
            Utc::now().signed_duration_since(start.with_timezone(&Utc)) < Duration::seconds(keep)
        });
    let store_payload = stored_count < keep_first.max(1) || !window_open;
    if store_payload {
        let hashes = user_hashes(app_key, event);
        let insert_user = statement(db, INSERT_USER);
        let mut raw = event.raw.clone();
        owlee_errors::scrub_raw_event(&mut raw);
        owlee_errors::limit_raw_event(&mut raw);
        let payload = serde_json::to_string(&raw).unwrap_or_else(|_| "{}".to_owned());
        let insert_event = statement(
            db,
            "INSERT INTO events (
                id, app_id, issue_id, timestamp, level, title, environment, release, user_key, payload
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        );
        sqlx::query(&insert_event)
            .bind(&event_id)
            .bind(&app.id)
            .bind(&issue_id)
            .bind(&event.timestamp)
            .bind(&event.level)
            .bind(&event.title)
            .bind(&event.environment)
            .bind(&event.release)
            .bind(hashes.first().cloned().unwrap_or_default())
            .bind(payload)
            .execute(&mut *tx)
            .await?;
        for hash in &hashes {
            sqlx::query(&insert_user)
                .bind(hash)
                .bind(&event_id)
                .execute(&mut *tx)
                .await?;
        }
        let next_stored = stored_count + 1;
        let next_window = if next_stored >= keep_first.max(1) {
            Some(now())
        } else {
            window_start
        };
        let mark = statement(
            db,
            "UPDATE issues SET stored_count = ?, sample_window_start = ? WHERE id = ?",
        );
        sqlx::query(&mark)
            .bind(next_stored)
            .bind(next_window)
            .bind(&issue_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Ingested {
        id: event_id,
        quota_exceeded: false,
        retry_after_secs: 0,
    })
}

fn app_from_row(row: &sqlx::any::AnyRow) -> Result<App, AppError> {
    Ok(App {
        id: row.try_get("id")?,
        project_id: crate::repo::parse_id(row, "project_id")?,
        name: row.try_get("name")?,
        slug: row.try_get("slug")?,
        public_key: row.try_get("public_key")?,
        platform: row.try_get("platform")?,
        created_at: row.try_get("created_at")?,
        daily_quota: row.try_get("daily_quota")?,
    })
}

pub async fn set_daily_quota(
    db: &Database,
    workspace_id: Id,
    id: &str,
    quota: Option<i64>,
) -> Result<App, AppError> {
    let app = get_app(db, workspace_id, id).await?;
    let sql = statement(db, "UPDATE error_apps SET daily_quota = ? WHERE id = ?");
    sqlx::query(&sql)
        .bind(quota)
        .bind(&app.id)
        .execute(db.pool())
        .await?;
    get_app(db, workspace_id, &app.id).await
}

fn issue_from_row(row: &sqlx::any::AnyRow) -> Result<IssueRow, AppError> {
    Ok(IssueRow {
        id: row.try_get("id")?,
        app_id: row.try_get("app_id")?,
        app_name: row.try_get("app_name")?,
        app_slug: row.try_get("app_slug")?,
        title: row.try_get("title")?,
        culprit: row.try_get("culprit")?,
        status: row.try_get("status")?,
        level: row.try_get("level")?,
        platform: row.try_get("platform")?,
        environment: row.try_get("environment")?,
        release: row.try_get("release")?,
        first_seen: row.try_get("first_seen")?,
        last_seen: row.try_get("last_seen")?,
        event_count: row.try_get("event_count")?,
        short_id: row
            .try_get::<Option<String>, _>("short_id")?
            .unwrap_or_default(),
        unhandled: row.try_get::<i64, _>("unhandled").unwrap_or(0) != 0,
        priority: priority_for(&row.try_get::<String, _>("level")?),
        user_count: 0,
        period_event_count: row.try_get("event_count")?,
        trend24h: vec![0; 24],
        trend14d: vec![0; 14],
    })
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if (ch == ' ' || ch == '-' || ch == '_') && !slug.ends_with('-') && !slug.is_empty()
        {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        "app".to_owned()
    } else {
        slug
    }
}

async fn unique_slug(db: &Database, base: &str) -> Result<String, AppError> {
    let mut candidate = base.to_owned();
    let mut suffix = 2;
    loop {
        let sql = statement(db, "SELECT id FROM error_apps WHERE slug = ?");
        let existing = sqlx::query(&sql)
            .bind(&candidate)
            .fetch_optional(db.pool())
            .await?;
        if existing.is_none() {
            return Ok(candidate);
        }
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
}

fn normalize_platform(platform: &str) -> String {
    let normalized = match platform.trim().to_ascii_lowercase().as_str() {
        "php" => "php".to_owned(),
        "javascript" | "browser" => "javascript".to_owned(),
        "node" | "nodejs" => "node".to_owned(),
        "laravel" => "laravel".to_owned(),
        "rust" => "rust".to_owned(),
        "python" => "python".to_owned(),
        "java" => "java".to_owned(),
        "go" | "golang" => "go".to_owned(),
        "ruby" => "ruby".to_owned(),
        "dotnet" | "csharp" => "dotnet".to_owned(),
        "" | "other" => "other".to_owned(),
        other => other
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .take(32)
            .collect(),
    };
    if normalized.is_empty() {
        "other".to_owned()
    } else {
        normalized
    }
}

fn like_pattern(search: &str) -> String {
    let mut escaped = String::new();
    for ch in search.trim().chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            escaped.push('\\');
        }
        escaped.push(ch.to_ascii_lowercase());
    }
    format!("%{escaped}%")
}

pub fn build_dsn(origin: &str, public_key: &str, app_id: &str) -> String {
    let origin = origin.trim().trim_end_matches('/');
    let (scheme, rest) = origin.split_once("://").unwrap_or(("http", origin));
    format!("{scheme}://{public_key}@{rest}/{app_id}")
}

/// Ids outside the workspace match nothing and are not counted.
pub async fn update_many_status(
    db: &Database,
    workspace_id: Id,
    ids: &[String],
    status: &str,
) -> Result<usize, AppError> {
    let status = issue_status(status)?;
    let sql = statement(
        db,
        &format!("UPDATE issues SET status = ? WHERE (id = ? OR short_id = ?) AND {IN_WORKSPACE}"),
    );
    let mut updated = 0;
    for id in ids {
        let result = sqlx::query(&sql)
            .bind(status)
            .bind(id)
            .bind(id)
            .bind(workspace_id.to_string())
            .execute(db.pool())
            .await?;
        updated += result.rows_affected() as usize;
    }
    Ok(updated)
}

pub async fn issue_analytics(
    db: &Database,
    issue_id: &str,
    range: &str,
    environment: &str,
    search: &str,
) -> Result<IssueAnalytics, AppError> {
    let sql = statement(
        db,
        "SELECT id, timestamp, level, title, environment, release, user_key, payload
         FROM events WHERE issue_id = ? ORDER BY timestamp DESC LIMIT 1000",
    );
    let rows = sqlx::query(&sql)
        .bind(issue_id)
        .fetch_all(db.pool())
        .await?;
    let start = since_time(range);
    let needle = search.trim().to_ascii_lowercase();
    let mut matched = Vec::new();
    for row in &rows {
        let environment_value: Option<String> = row.try_get("environment")?;
        let title: String = row.try_get("title")?;
        let timestamp: String = row.try_get("timestamp")?;
        if !environment.is_empty() && environment_value.as_deref() != Some(environment) {
            continue;
        }
        if !needle.is_empty() && !title.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        if let Some(start) = start {
            if let Some(parsed) = parse_time(&timestamp) {
                if parsed < start {
                    continue;
                }
            }
        }
        matched.push(row);
    }
    let bucket_count = bucket_count(range);
    let histogram_start = start.unwrap_or_else(|| Utc::now() - Duration::days(14));
    let mut histogram = vec![0i64; bucket_count];
    let mut users = std::collections::BTreeSet::new();
    let mut tags: std::collections::BTreeMap<String, std::collections::BTreeMap<String, i64>> =
        std::collections::BTreeMap::new();
    let mut summaries = Vec::new();
    let mut latest_payload = None;
    for row in &matched {
        let timestamp: String = row.try_get("timestamp")?;
        let user_key: Option<String> = row.try_get("user_key")?;
        let payload: String = row.try_get("payload")?;
        if let Some(user_key) = user_key.filter(|value| !value.is_empty()) {
            users.insert(user_key);
        }
        if let Some(parsed) = parse_time(&timestamp) {
            if let Some(index) = bucket_index(parsed, histogram_start, bucket_count) {
                histogram[index] += 1;
            }
        }
        let mut user = None;
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&payload) {
            let normalized = owlee_errors::normalize_event(raw);
            user = user_label(&normalized.user);
            for tag in normalized.tags {
                *tags
                    .entry(tag.key)
                    .or_default()
                    .entry(tag.value)
                    .or_default() += 1;
            }
        }
        if latest_payload.is_none() {
            latest_payload = Some(payload);
        }
        if summaries.len() < EVENT_LIST_LIMIT {
            summaries.push(EventSummary {
                id: row.try_get("id")?,
                timestamp,
                level: row.try_get("level")?,
                title: row.try_get("title")?,
                environment: row.try_get("environment")?,
                release: row.try_get("release")?,
                user,
            });
        }
    }
    let mut groups: Vec<(i64, TagGroup)> = tags
        .into_iter()
        .map(|(key, counts)| {
            let total = counts.values().sum();
            let mut values: Vec<TagValue> = counts
                .into_iter()
                .map(|(value, count)| TagValue { value, count })
                .collect();
            values.sort_by(|left, right| right.count.cmp(&left.count));
            values.truncate(TAG_VALUE_LIMIT);
            (total, TagGroup { key, values })
        })
        .collect();
    groups.sort_by(|left, right| right.0.cmp(&left.0));
    groups.truncate(TAG_KEY_LIMIT);
    Ok(IssueAnalytics {
        events: summaries,
        latest_payload,
        histogram,
        users: users.len() as i64,
        period_events: matched.len() as i64,
        tags: groups.into_iter().map(|(_, group)| group).collect(),
    })
}

fn user_label(user: &serde_json::Value) -> Option<String> {
    ["email", "username", "id"].iter().find_map(|key| {
        user.get(*key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserEvent {
    pub id: String,
    pub app_id: String,
    pub app_name: String,
    pub timestamp: String,
    pub title: String,
    pub payload: serde_json::Value,
}

/// Only events of the workspace's apps. The same person in another workspace stays untouched.
pub async fn events_for_user(
    db: &Database,
    workspace_id: Id,
    user_key: &str,
) -> Result<Vec<UserEvent>, AppError> {
    let sql = statement(
        db,
        "SELECT e.id, e.app_id, a.name AS app_name, e.timestamp, e.title, e.payload
         FROM events e
         JOIN error_apps a ON a.id = e.app_id
         JOIN projects pr ON pr.id = a.project_id
         WHERE e.id IN (SELECT event_id FROM event_users WHERE user_hash = ?)
           AND pr.workspace_id = ?
         ORDER BY e.timestamp",
    );
    let rows = sqlx::query(&sql)
        .bind(user_key)
        .bind(workspace_id.to_string())
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|row| {
            let payload: String = row.try_get("payload")?;
            Ok(UserEvent {
                id: row.try_get("id")?,
                app_id: row.try_get("app_id")?,
                app_name: row.try_get("app_name")?,
                timestamp: row.try_get("timestamp")?,
                title: row.try_get("title")?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
            })
        })
        .collect()
}

pub async fn delete_events_for_user(
    db: &Database,
    workspace_id: Id,
    user_key: &str,
) -> Result<u64, AppError> {
    let sql = statement(
        db,
        &format!(
            "DELETE FROM events WHERE id IN (SELECT event_id FROM event_users WHERE user_hash = ?)
             AND {IN_WORKSPACE}"
        ),
    );
    Ok(sqlx::query(&sql)
        .bind(user_key)
        .bind(workspace_id.to_string())
        .execute(db.pool())
        .await?
        .rows_affected())
}

/// Hashes `user_key` and scrubs `payload` on rows stored before either existed.
/// The privacy migration clears `user_key`, so each row runs once.
pub async fn privatize_events(db: &Database, app_key: &AppKey) -> Result<(), AppError> {
    let update = statement(
        db,
        "UPDATE events SET user_key = ?, payload = ? WHERE id = ?",
    );
    let insert_user = statement(db, INSERT_USER);
    loop {
        let rows = sqlx::query("SELECT id, payload FROM events WHERE user_key IS NULL LIMIT 500")
            .fetch_all(db.pool())
            .await?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            let id: String = row.try_get("id")?;
            let payload: String = row.try_get("payload")?;
            let (hashes, payload) = match serde_json::from_str::<serde_json::Value>(&payload) {
                Ok(mut raw) => {
                    let hashes = user_hashes(app_key, &owlee_errors::normalize_event(raw.clone()));
                    owlee_errors::scrub_raw_event(&mut raw);
                    (hashes, raw.to_string())
                }
                Err(_) => (Vec::new(), "{}".to_owned()),
            };
            for hash in &hashes {
                sqlx::query(&insert_user)
                    .bind(hash)
                    .bind(&id)
                    .execute(db.pool())
                    .await?;
            }
            sqlx::query(&update)
                .bind(hashes.first().cloned().unwrap_or_default())
                .bind(payload)
                .bind(id)
                .execute(db.pool())
                .await?;
        }
    }
}

const INSERT_USER: &str =
    "INSERT INTO event_users (user_hash, event_id) VALUES (?, ?) ON CONFLICT DO NOTHING";

async fn attach_trends(db: &Database, issues: &mut [IssueRow]) -> Result<(), AppError> {
    if issues.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let sql = statement(
        db,
        "SELECT issue_id, timestamp FROM events WHERE timestamp >= ?",
    );
    let rows = sqlx::query(&sql)
        .bind((now - Duration::days(14)).to_rfc3339_opts(SecondsFormat::Millis, true))
        .fetch_all(db.pool())
        .await?;
    let index: std::collections::HashMap<String, usize> = issues
        .iter()
        .enumerate()
        .map(|(position, issue)| (issue.id.clone(), position))
        .collect();
    for row in &rows {
        let issue_id: String = row.try_get("issue_id")?;
        let Some(&position) = index.get(&issue_id) else {
            continue;
        };
        let timestamp: String = row.try_get("timestamp")?;
        let Some(parsed) = parse_time(&timestamp) else {
            continue;
        };
        let issue = &mut issues[position];
        if let Some(bucket) = bucket_index(parsed, now - Duration::hours(24), 24) {
            issue.trend24h[bucket] += 1;
        }
        if let Some(bucket) = bucket_index(parsed, now - Duration::days(14), 14) {
            issue.trend14d[bucket] += 1;
        }
    }
    Ok(())
}

fn since_bound(spec: &str) -> String {
    since_time(spec)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn since_time(spec: &str) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    match spec {
        "1h" => Some(now - Duration::hours(1)),
        "24h" => Some(now - Duration::hours(24)),
        "7d" => Some(now - Duration::days(7)),
        "14d" => Some(now - Duration::days(14)),
        "30d" => Some(now - Duration::days(30)),
        _ => None,
    }
}

fn bucket_count(spec: &str) -> usize {
    match spec {
        "1h" => 12,
        "24h" => 24,
        "7d" => 7,
        "30d" => 30,
        _ => 14,
    }
}

fn bucket_index(timestamp: DateTime<Utc>, start: DateTime<Utc>, count: usize) -> Option<usize> {
    if count == 0 || timestamp < start {
        return None;
    }
    let span = Utc::now() - start;
    let width = (span.num_milliseconds().max(1) as f64) / count as f64;
    let offset = (timestamp - start).num_milliseconds().max(0) as f64;
    Some(((offset / width) as usize).min(count - 1))
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn priority_for(level: &str) -> String {
    match level {
        "fatal" => "high",
        "error" => "medium",
        _ => "low",
    }
    .to_owned()
}

fn short_prefix(slug: &str) -> String {
    let mut prefix: String = slug
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(16)
        .collect();
    prefix.make_ascii_uppercase();
    if prefix.is_empty() {
        "ISSUE".to_owned()
    } else {
        prefix
    }
}

fn event_unhandled(event: &NormalizedEvent) -> bool {
    event
        .exceptions
        .iter()
        .any(|exception| exception.handled == Some(false))
}

const USER_FIELDS: [&str; 4] = ["id", "email", "username", "ip_address"];

/// Hashes of every identity on the event, in `USER_FIELDS` order. The first one
/// is the event's `user_key`, which counts distinct users.
fn user_hashes(app_key: &AppKey, event: &NormalizedEvent) -> Vec<String> {
    let mut hashes: Vec<String> = Vec::new();
    for key in USER_FIELDS {
        if let Some(value) = event.user.get(key).and_then(|item| item.as_str()) {
            let value = value.trim();
            if !value.is_empty() && value != "{{auto}}" {
                let hash = hash_user_key(app_key, value);
                if !hashes.contains(&hash) {
                    hashes.push(hash);
                }
            }
        }
    }
    hashes
}

/// Keyed so the stored value cannot be reversed with a list of emails or addresses.
/// Lowercased so `Ada@Example.com` and `ada@example.com` match.
pub fn hash_user_key(app_key: &AppKey, value: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::new()
        .chain_update(app_key.bytes())
        .chain_update([0])
        .chain_update(value.trim().to_lowercase())
        .finalize();
    hex::encode(&digest[..16])
}
