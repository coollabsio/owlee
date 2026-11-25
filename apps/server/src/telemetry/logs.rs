//! Log search, facets, detail, live tail, and retention.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use owlee_platform::{Database, TimestampMillis};
use owlee_telemetry::{Field, Level, Query as Search, TermKind, parse_query};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use sqlx::any::AnyRow;

use super::sources;
use crate::error::AppError;
use crate::paging::{Cursor, Key, PageParams};
use crate::state::AppState;
use crate::workspaces::{Member, ProjectFilter};

const PAGE: i64 = 200;
const DEFAULT_RANGE_MS: i64 = 15 * 60 * 1000;
const MAX_RANGE_MS: i64 = 7 * 86_400_000;
const TOP_SERVICES: usize = 10;
/// Facets count only the newest rows of a search, so a wide range stays fast. The load test chose this cap.
const FACET_ROWS: i64 = 10_000;
const BATCH: u64 = 500;

#[derive(Deserialize, Default)]
pub struct LogParams {
    #[serde(default)]
    pub q: String,
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// A `tailCursor`: only rows stored after it.
    pub after: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogView {
    pub id: String,
    pub time_ms: i64,
    pub level: String,
    pub severity_text: String,
    pub service: String,
    pub environment: String,
    pub host: String,
    pub body: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub source_id: String,
    pub source_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub items: Vec<LogView>,
    pub next_cursor: Option<String>,
    pub tail_cursor: String,
}

#[derive(Serialize)]
pub struct LogDetail {
    #[serde(flatten)]
    pub log: LogView,
    pub attributes: Value,
    pub resource: Value,
}

#[derive(Serialize)]
pub struct LevelCount {
    pub level: &'static str,
    pub count: i64,
}

#[derive(Serialize)]
pub struct ServiceCount {
    pub service: String,
    pub count: i64,
}

#[derive(Serialize)]
pub struct Facets {
    pub levels: Vec<LevelCount>,
    pub services: Vec<ServiceCount>,
    /// True when the search has more rows than `FACET_ROWS` and the counts cover only the newest of them.
    pub capped: bool,
}

/// The rows one search reads: the visible sources, a time range, and the query.
struct Scope {
    sources: HashMap<String, String>,
    from: i64,
    /// `None` for live tail, which takes every row stored after its cursor.
    to: Option<i64>,
    search: Search,
}

impl Scope {
    async fn resolve(
        state: &AppState,
        member: &Member,
        project: &ProjectFilter,
        params: &LogParams,
        tail: bool,
    ) -> Result<Self, AppError> {
        let search = parse_query(&params.q).map_err(|bad| AppError::field("q", bad.message))?;
        let now = TimestampMillis::now().as_millis();
        let to = params.to.unwrap_or(now);
        let from = params.from.unwrap_or(to - DEFAULT_RANGE_MS);
        if from > to {
            return Err(AppError::field("to", "The end must be after the start."));
        }
        if to - from > MAX_RANGE_MS {
            return Err(AppError::field("to", "Use a time range of at most 7 days."));
        }
        let project = project.resolve(&state.db, member.workspace_id).await?;
        let sources = sources::list(&state.db, member.workspace_id, project)
            .await?
            .into_iter()
            .map(|source| (source.id, source.name))
            .collect();
        Ok(Self {
            sources,
            from,
            to: (!tail).then_some(to),
            search,
        })
    }
}

enum Bind {
    Text(String),
    Int(i64),
}

/// A WHERE clause and its values. No user text goes into the SQL string.
#[derive(Default)]
struct Filter {
    sql: Vec<String>,
    binds: Vec<Bind>,
}

impl Filter {
    fn bind(&mut self, value: Bind) -> String {
        self.binds.push(value);
        format!("${}", self.binds.len())
    }

    fn text(&mut self, value: &str) -> String {
        self.bind(Bind::Text(value.to_owned()))
    }

    fn of(scope: &Scope) -> Self {
        let mut filter = Self::default();
        let ids: Vec<String> = scope.sources.keys().map(|id| filter.text(id)).collect();
        filter
            .sql
            .push(format!("l.source_id IN ({})", ids.join(", ")));
        let from = filter.bind(Bind::Int(scope.from));
        filter.sql.push(format!("l.time_ms >= {from}"));
        if let Some(to) = scope.to {
            let to = filter.bind(Bind::Int(to));
            filter.sql.push(format!("l.time_ms <= {to}"));
        }
        for term in &scope.search.terms {
            let condition = filter.term(&term.kind, term.negated);
            filter.sql.push(condition);
        }
        filter
    }

    fn term(&mut self, kind: &TermKind, negated: bool) -> String {
        let condition = match kind {
            TermKind::Text(text) => {
                let pattern = self.text(&format!("%{}%", escape_like(text)));
                format!("LOWER(l.body) LIKE LOWER({pattern}) ESCAPE '\\'")
            }
            TermKind::Field(Field::Trace, id) if negated => {
                let id = self.text(id);
                return format!("COALESCE(l.trace_id, '') <> {id}");
            }
            TermKind::Field(field, value) => {
                let value = self.text(value);
                format!("l.{} = {value}", field.column())
            }
            TermKind::Level(level) => {
                let level = self.text(level.as_str());
                format!("l.level = {level}")
            }
            TermKind::LevelAtLeast(least) => {
                let levels: Vec<String> = Level::ALL
                    .into_iter()
                    .filter(|level| level >= least)
                    .map(|level| self.text(level.as_str()))
                    .collect();
                format!("l.level IN ({})", levels.join(", "))
            }
            TermKind::Attribute { key, value } => {
                let key = self.text(key);
                let value = self.text(value);
                format!(
                    "EXISTS (SELECT 1 FROM log_attributes a \
                     WHERE a.log_id = l.id AND a.key = {key} AND a.value = {value})"
                )
            }
        };
        if negated {
            format!("NOT ({condition})")
        } else {
            condition
        }
    }

    fn query<'q>(
        &self,
        sql: &'q str,
    ) -> sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments<'q>> {
        let mut query = sqlx::query(sql);
        for bind in &self.binds {
            query = match bind {
                Bind::Text(text) => query.bind(text.clone()),
                Bind::Int(number) => query.bind(*number),
            };
        }
        query
    }

    fn clause(&self) -> String {
        self.sql.join(" AND ")
    }
}

fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

const LOG_COLUMNS: &str = "l.id, l.time_ms, l.level, l.severity_text, l.service, l.environment, \
     l.host, l.body, l.trace_id, l.span_id, l.source_id";

fn log_from_row(row: &AnyRow, sources: &HashMap<String, String>) -> Result<LogView, sqlx::Error> {
    let source_id: String = row.try_get("source_id")?;
    Ok(LogView {
        id: row.try_get("id")?,
        time_ms: row.try_get("time_ms")?,
        level: row.try_get("level")?,
        severity_text: row.try_get("severity_text")?,
        service: row.try_get("service")?,
        environment: row.try_get("environment")?,
        host: row.try_get("host")?,
        body: row.try_get("body")?,
        trace_id: row.try_get("trace_id")?,
        span_id: row.try_get("span_id")?,
        source_name: sources.get(&source_id).cloned().unwrap_or_default(),
        source_id,
    })
}

/// The lowest UUIDv7 of this millisecond. Every row stored later sorts after it.
fn tail_floor(now_ms: i64) -> String {
    let hex = format!("{:012x}", now_ms.max(0));
    format!("{}-{}-7000-8000-000000000000", &hex[..8], &hex[8..])
}

fn parse_tail(raw: &str) -> Result<String, AppError> {
    uuid::Uuid::parse_str(raw)
        .map(|id| id.hyphenated().to_string())
        .map_err(|_| AppError::BadRequest("The cursor is not valid for this list".into()))
}

pub async fn search(
    State(state): State<AppState>,
    member: Member,
    Query(params): Query<LogParams>,
    Query(project): Query<ProjectFilter>,
    Query(page): Query<PageParams>,
) -> Result<Json<LogPage>, AppError> {
    let paging = page.parse(PAGE)?;
    let after = params.after.as_deref().map(parse_tail).transpose()?;
    let floor = tail_floor(TimestampMillis::now().as_millis());
    let scope = Scope::resolve(&state, &member, &project, &params, after.is_some()).await?;
    if scope.sources.is_empty() {
        return Ok(Json(LogPage {
            items: Vec::new(),
            next_cursor: None,
            tail_cursor: after.unwrap_or(floor),
        }));
    }
    let mut filter = Filter::of(&scope);
    let limit = match &after {
        Some(after) => {
            let after = filter.text(after);
            filter.sql.push(format!("l.id > {after}"));
            PAGE
        }
        None => {
            if let Some((time_ms, id)) = paging.int_after()? {
                let time = filter.bind(Bind::Int(time_ms));
                let id = filter.text(id);
                filter.sql.push(format!(
                    "(l.time_ms < {time} OR (l.time_ms = {time} AND l.id < {id}))"
                ));
            }
            paging.fetch()
        }
    };
    let sql = format!(
        "SELECT {LOG_COLUMNS} FROM logs l WHERE {} ORDER BY l.time_ms DESC, l.id DESC LIMIT {limit}",
        filter.clause()
    );
    let rows = filter.query(&sql).fetch_all(state.db.pool()).await?;
    let items = rows
        .iter()
        .map(|row| log_from_row(row, &scope.sources))
        .collect::<Result<Vec<_>, _>>()?;
    let newest = items.iter().map(|item| item.id.clone()).max();
    let tail_cursor = [after.clone().unwrap_or(floor), newest.unwrap_or_default()]
        .into_iter()
        .max()
        .unwrap_or_default();
    if after.is_some() {
        return Ok(Json(LogPage {
            items,
            next_cursor: None,
            tail_cursor,
        }));
    }
    let paged = paging.finish(items, |item| Cursor::new(Key::Int(item.time_ms), &item.id));
    Ok(Json(LogPage {
        items: paged.items,
        next_cursor: paged.next_cursor,
        tail_cursor,
    }))
}

pub async fn facets(
    State(state): State<AppState>,
    member: Member,
    Query(params): Query<LogParams>,
    Query(project): Query<ProjectFilter>,
) -> Result<Json<Facets>, AppError> {
    let scope = Scope::resolve(&state, &member, &project, &params, false).await?;
    let mut levels: Vec<LevelCount> = Level::ALL
        .into_iter()
        .map(|level| LevelCount {
            level: level.as_str(),
            count: 0,
        })
        .collect();
    if scope.sources.is_empty() {
        return Ok(Json(Facets {
            levels,
            services: Vec::new(),
            capped: false,
        }));
    }
    let filter = Filter::of(&scope);
    let sql = format!(
        "SELECT x.level, x.service, COUNT(*) AS n FROM \
         (SELECT l.level, l.service FROM logs l WHERE {} ORDER BY l.time_ms DESC LIMIT {FACET_ROWS}) x \
         GROUP BY x.level, x.service",
        filter.clause()
    );
    let mut by_service: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut total = 0;
    for row in filter.query(&sql).fetch_all(state.db.pool()).await? {
        let level: String = row.try_get("level")?;
        let service: String = row.try_get("service")?;
        let n: i64 = row.try_get("n")?;
        total += n;
        if let Some(entry) = levels.iter_mut().find(|entry| entry.level == level) {
            entry.count += n;
        }
        *by_service.entry(service).or_default() += n;
    }
    let mut services: Vec<ServiceCount> = by_service
        .into_iter()
        .map(|(service, count)| ServiceCount { service, count })
        .collect();
    services.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.service.cmp(&b.service)));
    services.truncate(TOP_SERVICES);
    Ok(Json(Facets {
        levels,
        services,
        capped: total >= FACET_ROWS,
    }))
}

/// Another workspace's log, or a deleted source's, is 404.
pub async fn detail(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<String>,
) -> Result<Json<LogDetail>, AppError> {
    let row = sqlx::query(&format!(
        "SELECT {LOG_COLUMNS}, l.attributes_json, l.resource_json, s.name AS source_name \
         FROM logs l JOIN telemetry_sources s ON s.id = l.source_id JOIN projects p ON p.id = s.project_id \
         WHERE l.id = $1 AND p.workspace_id = $2 AND s.deleted_at IS NULL"
    ))
    .bind(&id)
    .bind(member.workspace_id.to_string())
    .fetch_optional(state.db.pool())
    .await?
    .ok_or(AppError::NotFound)?;
    let source_id: String = row.try_get("source_id")?;
    let sources = HashMap::from([(source_id, row.try_get::<String, _>("source_name")?)]);
    let json = |column: &str| -> Result<Value, sqlx::Error> {
        let text: String = row.try_get(column)?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    };
    Ok(Json(LogDetail {
        log: log_from_row(&row, &sources)?,
        attributes: json("attributes_json")?,
        resource: json("resource_json")?,
    }))
}

/// Logs older than `before_ms`, and every log of a deleted source and then
/// the source itself, in batches of 500 so no statement holds the database long.
/// Returns the number of logs deleted.
pub async fn prune(db: &Database, before_ms: i64) -> Result<u64, sqlx::Error> {
    let sources = sqlx::query("SELECT id, deleted_at FROM telemetry_sources")
        .fetch_all(db.pool())
        .await?;
    let mut total = 0;
    for row in &sources {
        let id: String = row.try_get("id")?;
        let deleted: Option<i64> = row.try_get("deleted_at")?;
        // i64::MAX takes every log of a deleted source.
        let before = if deleted.is_some() {
            i64::MAX
        } else {
            before_ms
        };
        loop {
            let count = sqlx::query(
                "DELETE FROM logs WHERE id IN \
                 (SELECT id FROM logs WHERE source_id = $1 AND time_ms < $2 LIMIT 500)",
            )
            .bind(&id)
            .bind(before)
            .execute(db.pool())
            .await?
            .rows_affected();
            total += count;
            if count < BATCH {
                break;
            }
        }
        if deleted.is_some() {
            sqlx::query("DELETE FROM telemetry_sources WHERE id = $1")
                .bind(&id)
                .execute(db.pool())
                .await?;
        }
    }
    Ok(total)
}
