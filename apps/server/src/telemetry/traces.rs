//! Services, the service page, the trace list, and the trace page. The service
//! numbers come from `span_rollups`; percentiles are read from its buckets.

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, Query, State};
use owlee_platform::{Database, Id};
use owlee_telemetry::{
    BUCKETS, Dataset, Query as Search, TermKind, operation, parse_query, percentile_ms,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use sqlx::any::AnyRow;

use super::filter::{Filter, contains_pattern, time_range, visible_sources};
use super::logs::{LOG_COLUMNS, LogView, log_from_row};
use crate::error::AppError;
use crate::paging::{Cursor, Key, PageParams};
use crate::state::AppState;
use crate::workspaces::{Member, ProjectFilter};

const DAY_MS: i64 = 86_400_000;
const TRACE_RANGE_MS: i64 = 15 * 60 * 1000;
const MAX_TRACE_RANGE_MS: i64 = 7 * DAY_MS;
const MAX_SERVICE_RANGE_MS: i64 = 90 * DAY_MS;
const PAGE: i64 = 200;
const MAX_POINTS: i64 = 300;
const MAX_SPANS: usize = 2000;
const TRACE_LOGS: i64 = 200;
const TRACE_ERRORS: i64 = 200;

#[derive(Deserialize, Default)]
pub struct RangeParams {
    pub from: Option<i64>,
    pub to: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct ServiceParams {
    pub source: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct TraceParams {
    #[serde(default)]
    pub q: String,
    pub from: Option<i64>,
    pub to: Option<i64>,
}

/// Request counts and the latency histogram of some rollup rows.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub requests: i64,
    pub errors: i64,
    pub error_rate: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl Stats {
    fn of(row: &AnyRow) -> Result<Self, sqlx::Error> {
        let requests: i64 = row.try_get("requests")?;
        let errors: i64 = row.try_get("errors")?;
        let mut buckets = [0; BUCKETS];
        for (index, bucket) in buckets.iter_mut().enumerate() {
            *bucket = row.try_get(format!("b{index:02}").as_str())?;
        }
        Ok(Self {
            requests,
            errors,
            error_rate: if requests > 0 {
                errors as f64 / requests as f64
            } else {
                0.0
            },
            p50_ms: percentile_ms(&buckets, 0.50),
            p95_ms: percentile_ms(&buckets, 0.95),
            p99_ms: percentile_ms(&buckets, 0.99),
        })
    }

    const EMPTY: Self = Self {
        requests: 0,
        errors: 0,
        error_rate: 0.0,
        p50_ms: 0.0,
        p95_ms: 0.0,
        p99_ms: 0.0,
    };
}

/// `requests`, `errors`, and `b00` .. `b19` summed over the grouped rows of `r`.
/// `SUM` of a BIGINT is NUMERIC on Postgres, so it is cast back.
fn sums() -> String {
    let mut columns = vec![
        "CAST(SUM(r.count) AS BIGINT) AS requests".to_owned(),
        "CAST(SUM(r.errors) AS BIGINT) AS errors".to_owned(),
    ];
    columns.extend(
        (0..BUCKETS).map(|index| format!("CAST(SUM(r.b{index:02}) AS BIGINT) AS b{index:02}")),
    );
    columns.join(", ")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceItem {
    pub service: String,
    pub source_id: String,
    pub source_name: String,
    #[serde(flatten)]
    pub stats: Stats,
}

#[derive(Serialize)]
pub struct ServiceList {
    pub items: Vec<ServiceItem>,
}

/// One row for each (source, service) with entry spans in the range, most requests first.
pub async fn services(
    State(state): State<AppState>,
    member: Member,
    Query(params): Query<RangeParams>,
    Query(project): Query<ProjectFilter>,
) -> Result<Json<ServiceList>, AppError> {
    let (from, to) = time_range(params.from, params.to, DAY_MS, MAX_SERVICE_RANGE_MS)?;
    let sources = visible_sources(&state, &member, &project).await?;
    if sources.is_empty() {
        return Ok(Json(ServiceList { items: Vec::new() }));
    }
    let mut filter = Filter::default();
    filter.sources("r.source_id", &sources);
    filter.sql.push("r.operation = ''".to_owned());
    minutes(&mut filter, from, to);
    let sql = format!(
        "SELECT r.source_id, r.service, {} FROM span_rollups r WHERE {} GROUP BY r.source_id, r.service",
        sums(),
        filter.clause()
    );
    let mut items = Vec::new();
    for row in filter.query(&sql).fetch_all(state.db.pool()).await? {
        let source_id: String = row.try_get("source_id")?;
        items.push(ServiceItem {
            service: row.try_get("service")?,
            source_name: sources.get(&source_id).cloned().unwrap_or_default(),
            source_id,
            stats: Stats::of(&row)?,
        });
    }
    items.sort_by(|a, b| {
        b.stats
            .requests
            .cmp(&a.stats.requests)
            .then_with(|| a.service.cmp(&b.service))
            .then_with(|| a.source_id.cmp(&b.source_id))
    });
    Ok(Json(ServiceList { items }))
}

fn minutes(filter: &mut Filter, from_ms: i64, to_ms: i64) {
    let from = filter.int(from_ms.div_euclid(60_000));
    let to = filter.int(to_ms.div_euclid(60_000));
    filter
        .sql
        .push(format!("r.minute >= {from} AND r.minute <= {to}"));
}

fn service_rows(filter: &mut Filter, source_id: &str, service: &str, from: i64, to: i64) {
    let source = filter.text(source_id);
    let name = filter.text(service);
    filter
        .sql
        .push(format!("r.source_id = {source} AND r.service = {name}"));
    minutes(filter, from, to);
}

#[derive(Serialize)]
pub struct Point {
    /// The start of the bucket, Unix milliseconds.
    pub t: i64,
    #[serde(flatten)]
    pub stats: Stats,
}

#[derive(Serialize)]
pub struct OperationItem {
    pub operation: String,
    #[serde(flatten)]
    pub stats: Stats,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDetail {
    pub service: String,
    pub source_id: String,
    pub bucket_ms: i64,
    pub series: Vec<Point>,
    pub operations: Vec<OperationItem>,
}

/// Charts and operations of one service. A bucket is one minute, or more so that the
/// series has at most 300 points. A bucket with no requests has zeroes.
pub async fn service(
    State(state): State<AppState>,
    member: Member,
    Path(service): Path<String>,
    Query(params): Query<ServiceParams>,
    Query(project): Query<ProjectFilter>,
) -> Result<Json<ServiceDetail>, AppError> {
    let (from, to) = time_range(params.from, params.to, DAY_MS, MAX_SERVICE_RANGE_MS)?;
    let sources = visible_sources(&state, &member, &project).await?;
    let source_id = match params.source.filter(|id| !id.is_empty()) {
        Some(id) if sources.contains_key(&id) => id,
        Some(_) => return Err(AppError::NotFound),
        None if sources.is_empty() => return Err(AppError::NotFound),
        None => {
            let mut filter = Filter::default();
            filter.sources("r.source_id", &sources);
            let name = filter.text(&service);
            filter
                .sql
                .push(format!("r.service = {name} AND r.operation = ''"));
            let sql = format!(
                "SELECT DISTINCT r.source_id FROM span_rollups r WHERE {}",
                filter.clause()
            );
            let mut found = filter.query(&sql).fetch_all(state.db.pool()).await?;
            match found.len() {
                0 => return Err(AppError::NotFound),
                1 => found.remove(0).try_get("source_id")?,
                _ => {
                    return Err(AppError::field(
                        "source",
                        "More than one source has this service. Choose a source.",
                    ));
                }
            }
        }
    };
    let first_minute = from.div_euclid(60_000);
    let span = to.div_euclid(60_000) - first_minute + 1;
    let bucket = ((span + MAX_POINTS - 1) / MAX_POINTS).max(1);

    let mut filter = Filter::default();
    let start = filter.int(first_minute);
    let size = filter.int(bucket);
    service_rows(&mut filter, &source_id, &service, from, to);
    filter.sql.push("r.operation = ''".to_owned());
    let sql = format!(
        "SELECT (r.minute - {start}) / {size} AS slot, {} FROM span_rollups r WHERE {} GROUP BY slot",
        sums(),
        filter.clause()
    );
    let mut slots: HashMap<i64, Stats> = HashMap::new();
    for row in filter.query(&sql).fetch_all(state.db.pool()).await? {
        slots.insert(row.try_get("slot")?, Stats::of(&row)?);
    }
    let series = (0..(span + bucket - 1) / bucket)
        .map(|slot| Point {
            t: (first_minute + slot * bucket) * 60_000,
            stats: slots.remove(&slot).unwrap_or(Stats::EMPTY),
        })
        .collect();

    let mut filter = Filter::default();
    service_rows(&mut filter, &source_id, &service, from, to);
    filter.sql.push("r.operation <> ''".to_owned());
    let sql = format!(
        "SELECT r.operation, {} FROM span_rollups r WHERE {} GROUP BY r.operation",
        sums(),
        filter.clause()
    );
    let mut operations = Vec::new();
    for row in filter.query(&sql).fetch_all(state.db.pool()).await? {
        operations.push(OperationItem {
            operation: row.try_get("operation")?,
            stats: Stats::of(&row)?,
        });
    }
    operations.sort_by(|a, b| {
        b.stats
            .requests
            .cmp(&a.stats.requests)
            .then_with(|| a.operation.cmp(&b.operation))
    });
    Ok(Json(ServiceDetail {
        service,
        source_id,
        bucket_ms: bucket * 60_000,
        series,
        operations,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceItem {
    pub trace_id: String,
    pub span_id: String,
    pub service: String,
    pub name: String,
    pub operation: String,
    pub start_ms: f64,
    pub duration_ms: f64,
    pub status: String,
    pub http_method: Option<String>,
    pub http_route: Option<String>,
    pub http_status: Option<i64>,
    pub source_id: String,
    pub source_name: String,
    #[serde(skip)]
    start_us: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TracePage {
    pub items: Vec<TraceItem>,
    pub next_cursor: Option<String>,
}

const SPAN_COLUMNS: &str = "s.source_id, s.trace_id, s.span_id, s.parent_span_id, s.service, \
     s.name, s.kind, s.start_us, s.duration_us, s.status, s.status_message, s.http_method, \
     s.http_route, s.http_status";

fn trace_item(row: &AnyRow, sources: &HashMap<String, String>) -> Result<TraceItem, sqlx::Error> {
    let source_id: String = row.try_get("source_id")?;
    let name: String = row.try_get("name")?;
    let http_method: Option<String> = row.try_get("http_method")?;
    let http_route: Option<String> = row.try_get("http_route")?;
    let start_us: i64 = row.try_get("start_us")?;
    let duration_us: i64 = row.try_get("duration_us")?;
    Ok(TraceItem {
        trace_id: row.try_get("trace_id")?,
        span_id: row.try_get("span_id")?,
        service: row.try_get("service")?,
        operation: operation(&name, http_method.as_deref(), http_route.as_deref()),
        name,
        start_ms: start_us as f64 / 1000.0,
        duration_ms: duration_us as f64 / 1000.0,
        status: row.try_get("status")?,
        http_method,
        http_route,
        http_status: row.try_get("http_status")?,
        source_name: sources.get(&source_id).cloned().unwrap_or_default(),
        source_id,
        start_us,
    })
}

fn span_term(filter: &mut Filter, kind: &TermKind, negated: bool) -> String {
    let condition = match kind {
        TermKind::Text(text) => {
            let pattern = filter.text(&contains_pattern(text));
            format!("LOWER(s.name) LIKE LOWER({pattern}) ESCAPE '\\'")
        }
        TermKind::Field(field, value) => {
            let value = filter.text(value);
            format!("s.{} = {value}", field.column())
        }
        TermKind::Duration(compare, micros) => {
            let micros = filter.int(*micros);
            format!("s.duration_us {} {micros}", compare.sql())
        }
        TermKind::Status(status) => {
            let status = filter.text(status.as_str());
            format!("s.status = {status}")
        }
        TermKind::Kind(span_kind) => {
            let span_kind = filter.text(span_kind.as_str());
            format!("s.kind = {span_kind}")
        }
        TermKind::Attribute { key, value } => {
            let key = filter.text(key);
            let value = filter.text(value);
            format!(
                "EXISTS (SELECT 1 FROM span_attributes a WHERE a.source_id = s.source_id \
                 AND a.trace_id = s.trace_id AND a.span_id = s.span_id \
                 AND a.key = {key} AND a.value = {value})"
            )
        }
        // The parser refuses `level:` on traces.
        TermKind::Level(_) | TermKind::LevelAtLeast(_) => "1 = 0".to_owned(),
    };
    if negated {
        format!("NOT ({condition})")
    } else {
        condition
    }
}

/// The cursor id of a span: trace id, span id, then source id.
fn cursor_id(item: &TraceItem) -> String {
    format!("{}{}{}", item.trace_id, item.span_id, item.source_id)
}

/// Entry spans, newest first.
pub async fn list(
    State(state): State<AppState>,
    member: Member,
    Query(params): Query<TraceParams>,
    Query(project): Query<ProjectFilter>,
    Query(page): Query<PageParams>,
) -> Result<Json<TracePage>, AppError> {
    let paging = page.parse(PAGE)?;
    let search: Search =
        parse_query(&params.q, Dataset::Traces).map_err(|bad| AppError::field("q", bad.message))?;
    let (from, to) = time_range(params.from, params.to, TRACE_RANGE_MS, MAX_TRACE_RANGE_MS)?;
    let sources = visible_sources(&state, &member, &project).await?;
    if sources.is_empty() {
        return Ok(Json(TracePage {
            items: Vec::new(),
            next_cursor: None,
        }));
    }
    let mut filter = Filter::default();
    filter.sources("s.source_id", &sources);
    let from = filter.int(from.saturating_mul(1000));
    let to = filter.int(to.saturating_mul(1000).saturating_add(999));
    filter.sql.push(format!(
        "s.is_entry = 1 AND s.start_us >= {from} AND s.start_us <= {to}"
    ));
    for term in &search.terms {
        let condition = span_term(&mut filter, &term.kind, term.negated);
        filter.sql.push(condition);
    }
    if let Some((start_us, id)) = paging.int_after()? {
        let bad = || AppError::BadRequest("The cursor is not valid for this list".into());
        let (trace_id, rest) = id.split_at_checked(32).ok_or_else(bad)?;
        let (span_id, source_id) = rest.split_at_checked(16).ok_or_else(bad)?;
        let start = filter.int(start_us);
        let trace = filter.text(trace_id);
        let span = filter.text(span_id);
        let source = filter.text(source_id);
        filter.sql.push(format!(
            "(s.start_us, s.trace_id, s.span_id, s.source_id) < ({start}, {trace}, {span}, {source})"
        ));
    }
    let sql = format!(
        "SELECT {SPAN_COLUMNS} FROM spans s WHERE {} \
         ORDER BY s.start_us DESC, s.trace_id DESC, s.span_id DESC, s.source_id DESC LIMIT {}",
        filter.clause(),
        paging.fetch()
    );
    let items = filter
        .query(&sql)
        .fetch_all(state.db.pool())
        .await?
        .iter()
        .map(|row| trace_item(row, &sources))
        .collect::<Result<Vec<_>, _>>()?;
    let paged = paging.finish(items, |item| {
        Cursor::new(Key::Int(item.start_us), cursor_id(item))
    });
    Ok(Json(TracePage {
        items: paged.items,
        next_cursor: paged.next_cursor,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanView {
    pub span_id: String,
    pub parent_span_id: String,
    /// The parent is named but not stored, so the waterfall shows the span under the root.
    pub parent_missing: bool,
    pub service: String,
    pub name: String,
    pub kind: String,
    pub start_ms: f64,
    pub duration_ms: f64,
    pub status: String,
    pub status_message: String,
    pub http_method: Option<String>,
    pub http_route: Option<String>,
    pub http_status: Option<i64>,
    pub attributes: Value,
    pub resource: Value,
    pub events: Value,
    pub links: Value,
    pub source_id: String,
    pub source_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceError {
    pub event_id: String,
    pub issue_id: String,
    pub title: String,
    pub level: String,
    pub timestamp: String,
    pub app_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceDetail {
    pub trace_id: String,
    pub start_ms: f64,
    pub duration_ms: f64,
    /// True when the trace has more than 2000 spans and only the first are listed.
    pub truncated: bool,
    pub spans: Vec<SpanView>,
    pub logs: Vec<LogView>,
    pub errors: Vec<TraceError>,
}

/// The spans, logs, and error events of a trace in the member's workspace, under the
/// project filter. 404 when none of them is visible.
pub async fn detail(
    State(state): State<AppState>,
    member: Member,
    Path(trace_id): Path<String>,
    Query(project): Query<ProjectFilter>,
) -> Result<Json<TraceDetail>, AppError> {
    let trace_id = trace_id.to_ascii_lowercase();
    if trace_id.len() != 32 || !trace_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::NotFound);
    }
    let project_id = project.resolve(&state.db, member.workspace_id).await?;
    let sources = visible_sources(&state, &member, &project).await?;
    let (mut spans, mut logs) = (Vec::new(), Vec::new());
    if !sources.is_empty() {
        let mut filter = Filter::default();
        let id = filter.text(&trace_id);
        filter.sql.push(format!("s.trace_id = {id}"));
        filter.sources("s.source_id", &sources);
        let sql = format!(
            "SELECT {SPAN_COLUMNS}, s.attributes_json, s.resource_json, s.events_json, s.links_json \
             FROM spans s WHERE {} ORDER BY s.start_us, s.span_id LIMIT {}",
            filter.clause(),
            MAX_SPANS + 1
        );
        spans = filter.query(&sql).fetch_all(state.db.pool()).await?;

        let mut filter = Filter::default();
        let id = filter.text(&trace_id);
        filter.sql.push(format!("l.trace_id = {id}"));
        filter.sources("l.source_id", &sources);
        let sql = format!(
            "SELECT {LOG_COLUMNS} FROM logs l WHERE {} ORDER BY l.time_ms, l.id LIMIT {TRACE_LOGS}",
            filter.clause()
        );
        logs = filter
            .query(&sql)
            .fetch_all(state.db.pool())
            .await?
            .iter()
            .map(|row| log_from_row(row, &sources))
            .collect::<Result<_, _>>()?;
    }
    let errors = trace_errors(&state.db, member.workspace_id, project_id, &trace_id).await?;
    if spans.is_empty() && logs.is_empty() && errors.is_empty() {
        return Err(AppError::NotFound);
    }
    let truncated = spans.len() > MAX_SPANS;
    spans.truncate(MAX_SPANS);
    let json = |row: &AnyRow, column: &str| -> Result<Value, sqlx::Error> {
        let text: String = row.try_get(column)?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    };
    let mut views = Vec::with_capacity(spans.len());
    for row in &spans {
        let item = trace_item(row, &sources)?;
        views.push((
            item.start_us,
            row.try_get::<i64, _>("duration_us")?,
            SpanView {
                span_id: item.span_id,
                parent_span_id: row.try_get("parent_span_id")?,
                parent_missing: false,
                service: item.service,
                name: item.name,
                kind: row.try_get("kind")?,
                start_ms: item.start_ms,
                duration_ms: item.duration_ms,
                status: item.status,
                status_message: row.try_get("status_message")?,
                http_method: item.http_method,
                http_route: item.http_route,
                http_status: item.http_status,
                attributes: json(row, "attributes_json")?,
                resource: json(row, "resource_json")?,
                events: json(row, "events_json")?,
                links: json(row, "links_json")?,
                source_id: item.source_id,
                source_name: item.source_name,
            },
        ));
    }
    let ids: HashSet<String> = views
        .iter()
        .map(|(_, _, span)| span.span_id.clone())
        .collect();
    for (_, _, span) in &mut views {
        span.parent_missing =
            !span.parent_span_id.is_empty() && !ids.contains(&span.parent_span_id);
    }
    let start_us = views.iter().map(|(start, _, _)| *start).min();
    let end_us = views
        .iter()
        .map(|(start, duration, _)| start.saturating_add(*duration))
        .max();
    let (start_ms, duration_ms) = match (start_us, end_us) {
        (Some(start), Some(end)) => (start as f64 / 1000.0, (end - start) as f64 / 1000.0),
        _ => (logs.first().map_or(0.0, |log| log.time_ms as f64), 0.0),
    };
    Ok(Json(TraceDetail {
        trace_id,
        start_ms,
        duration_ms,
        truncated,
        spans: views.into_iter().map(|(_, _, span)| span).collect(),
        logs,
        errors,
    }))
}

/// Error events of the trace in the workspace's apps, optionally of one project.
async fn trace_errors(
    db: &Database,
    workspace_id: Id,
    project_id: Option<Id>,
    trace_id: &str,
) -> Result<Vec<TraceError>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT e.id, e.issue_id, e.title, e.level, e.timestamp, e.app_id FROM events e \
         JOIN error_apps a ON a.id = e.app_id JOIN projects p ON p.id = a.project_id \
         WHERE e.trace_id = $1 AND p.workspace_id = $2 AND ($3 = '' OR a.project_id = $3) \
         ORDER BY e.timestamp, e.id LIMIT {TRACE_ERRORS}"
    ))
    .bind(trace_id)
    .bind(workspace_id.to_string())
    .bind(project_id.map(|id| id.to_string()).unwrap_or_default())
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|row| {
            Ok(TraceError {
                event_id: row.try_get("id")?,
                issue_id: row.try_get("issue_id")?,
                title: row.try_get("title")?,
                level: row.try_get("level")?,
                timestamp: row.try_get("timestamp")?,
                app_id: row.try_get("app_id")?,
            })
        })
        .collect()
}

/// Whether a live source of the workspace has spans of the trace, for `traceSpans` on events.
pub async fn trace_has_spans(
    db: &Database,
    workspace_id: Id,
    trace_id: &str,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        "SELECT 1 AS found FROM spans s JOIN telemetry_sources t ON t.id = s.source_id \
         JOIN projects p ON p.id = t.project_id \
         WHERE s.trace_id = $1 AND p.workspace_id = $2 AND t.deleted_at IS NULL LIMIT 1",
    )
    .bind(trace_id)
    .bind(workspace_id.to_string())
    .fetch_optional(db.pool())
    .await?;
    Ok(row.is_some())
}
