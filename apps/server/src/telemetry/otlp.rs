//! OTLP/HTTP intake: `POST /otlp/v1/{logs,traces,metrics}` (D6). Logs and
//! spans are stored. Metrics answer 200 with every item rejected in
//! `partial_success` until their phase.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::response::Response;
use owlee_errors::{BodyError, decode_body};
use owlee_platform::{Backend, TimestampMillis};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use owlee_telemetry::{
    BUCKETS, DecodeError, Encoding, LogRow, Signal, SpanRow, SpanStatus, bucket_index,
    count_points, decode_logs, decode_traces, export_response, normalize_logs, normalize_spans,
    status_body,
};
use sqlx::Row;

use super::sources::{self, Source, intake_key};
use crate::intake::DropReason;
use crate::peer::Client;
use crate::state::AppState;

/// Rows of one multi-row `INSERT`.
const INSERT_ROWS: usize = 500;
const LOG_COLUMNS: usize = 13;
const SPAN_COLUMNS: usize = 21;
/// Operations of one service in the rollup. More go to `(other)`.
const MAX_OPERATIONS: usize = 200;
const OTHER_OPERATION: &str = "(other)";

pub async fn logs(State(state): State<AppState>, request: Request) -> Response {
    intake(&state, request, Signal::Logs).await
}

pub async fn traces(State(state): State<AppState>, request: Request) -> Response {
    intake(&state, request, Signal::Traces).await
}

pub async fn metrics(State(state): State<AppState>, request: Request) -> Response {
    intake(&state, request, Signal::Metrics).await
}

/// Guards in order of cost: client bucket, token, source bucket, body size, body format, quota.
async fn intake(state: &AppState, request: Request, signal: Signal) -> Response {
    let headers = request.headers().clone();
    let Some(encoding) =
        header_str(&headers, header::CONTENT_TYPE).and_then(Encoding::from_content_type)
    else {
        return error(
            Encoding::Json,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "use Content-Type application/x-protobuf or application/json",
        );
    };
    let client = Client::of(&state.config, &headers, request.extensions());
    let by_ip = state
        .otlp_ip_limit
        .check(&format!("otlp-ip:{}", client.limit_key));
    if !by_ip.allowed {
        return limited(encoding, by_ip.retry_after_secs, "rate limit exceeded");
    }
    let source = match bearer(&headers) {
        Some(token) => match sources::for_token(&state.db, token).await {
            Ok(source) => source,
            Err(failure) => {
                tracing::error!(error = %failure, "failed to read telemetry source");
                return error(
                    encoding,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error",
                );
            }
        },
        None => None,
    };
    let Some(source) = source else {
        return error(
            encoding,
            StatusCode::UNAUTHORIZED,
            "invalid or missing bearer token",
        );
    };
    let key = intake_key(&source.id, signal);
    let by_source = state
        .otlp_source_limit
        .check(&format!("otlp-source:{}", source.id));
    if !by_source.allowed {
        // The items are not counted before the body is read, so the request counts as one.
        state
            .telemetry_intake
            .dropped(&key, DropReason::SourceRateLimit);
        return limited(encoding, by_source.retry_after_secs, "rate limit exceeded");
    }
    let body = match read_body(state, &headers, request).await {
        Ok(body) => body,
        Err(response) => {
            state.telemetry_intake.dropped(&key, DropReason::Malformed);
            return response(encoding);
        }
    };
    let decoded = match signal {
        Signal::Logs => decode_logs(&body, encoding).map(|request| {
            let rows = normalize_logs(&request, TimestampMillis::now().as_millis());
            (Batch::Logs(rows), 0)
        }),
        Signal::Traces => decode_traces(&body, encoding).map(|request| {
            let (rows, invalid) = normalize_spans(&request, TimestampMillis::now().as_millis());
            (Batch::Spans(rows), invalid)
        }),
        Signal::Metrics => {
            return reject_all(state, &key, count_points(&body, encoding), encoding);
        }
    };
    match decoded {
        Ok((batch, invalid)) => store(state, &source, &key, encoding, batch, invalid).await,
        Err(decode) => {
            state.telemetry_intake.dropped(&key, DropReason::Malformed);
            error(encoding, StatusCode::BAD_REQUEST, &decode.to_string())
        }
    }
}

fn reject_all(
    state: &AppState,
    key: &str,
    counted: Result<u64, DecodeError>,
    encoding: Encoding,
) -> Response {
    let count = match counted {
        Ok(count) => count,
        Err(decode) => {
            state.telemetry_intake.dropped(key, DropReason::Malformed);
            return error(encoding, StatusCode::BAD_REQUEST, &decode.to_string());
        }
    };
    if count > 0 {
        state
            .telemetry_intake
            .dropped_many(key, count, DropReason::Unsupported);
    }
    ok(
        encoding,
        export_response(
            Signal::Metrics,
            encoding,
            count,
            "metrics are not stored yet",
        ),
    )
}

/// The normalized items of one request.
enum Batch {
    Logs(Vec<LogRow>),
    Spans(Vec<SpanRow>),
}

impl Batch {
    fn len(&self) -> usize {
        match self {
            Self::Logs(rows) => rows.len(),
            Self::Spans(rows) => rows.len(),
        }
    }

    const fn signal(&self) -> Signal {
        match self {
            Self::Logs(_) => Signal::Logs,
            Self::Spans(_) => Signal::Traces,
        }
    }

    /// The `ingest_usage` kind.
    const fn quota_kind(&self) -> &'static str {
        match self {
            Self::Logs(_) => "log",
            Self::Spans(_) => "span",
        }
    }

    const fn quota_message(&self) -> &'static str {
        match self {
            Self::Logs(_) => "daily log quota reached",
            Self::Spans(_) => "daily span quota reached",
        }
    }

    /// A source's own cap applies to logs. Spans have the instance cap.
    fn quota(&self, state: &AppState, source: &Source) -> i64 {
        match self {
            Self::Logs(_) => source
                .daily_quota
                .unwrap_or(state.config.ingest.log_daily_quota),
            Self::Spans(_) => state.config.ingest.span_daily_quota,
        }
    }
}

/// Stores what the quota allows. `invalid` items were dropped by normalization.
async fn store(
    state: &AppState,
    source: &Source,
    key: &str,
    encoding: Encoding,
    batch: Batch,
    invalid: u64,
) -> Response {
    let signal = batch.signal();
    if invalid > 0 {
        state
            .telemetry_intake
            .dropped_many(key, invalid, DropReason::Malformed);
    }
    let invalid_message = "invalid trace or span id";
    let total = batch.len();
    if total == 0 {
        return ok(
            encoding,
            export_response(signal, encoding, invalid, invalid_message),
        );
    }
    let quota_message = batch.quota_message();
    let stored = match write(state, source, batch).await {
        Ok(stored) => stored,
        Err(failure) => {
            state
                .telemetry_intake
                .dropped_many(key, total as u64, DropReason::StorageFailed);
            tracing::error!(error = %failure, ?signal, "failed to store telemetry");
            return error(
                encoding,
                StatusCode::SERVICE_UNAVAILABLE,
                "failed to store telemetry",
            );
        }
    };
    let over = (total - stored) as u64;
    if stored > 0 {
        state.telemetry_intake.stored_many(key, stored as u64);
    }
    if over > 0 {
        state
            .telemetry_intake
            .dropped_many(key, over, DropReason::DailyQuota);
    }
    if stored == 0 {
        return limited(
            encoding,
            crate::quota::secs_until_utc_midnight(),
            quota_message,
        );
    }
    let message = if over > 0 {
        quota_message
    } else {
        invalid_message
    };
    ok(
        encoding,
        export_response(signal, encoding, over + invalid, message),
    )
}

/// Reserves quota, then stores the granted items. Returns how many were stored;
/// the rest are over the quota. A failed write gives its reservation back.
async fn write(state: &AppState, source: &Source, batch: Batch) -> Result<usize, sqlx::Error> {
    let day = crate::quota::today();
    let kind = batch.quota_kind();
    let quota = batch.quota(state, source);
    let granted = reserve(state, kind, &source.id, &day, batch.len(), quota).await?;
    if granted == 0 {
        return Ok(0);
    }
    let written = match &batch {
        Batch::Logs(rows) => insert_logs(state, source, &rows[..granted]).await,
        Batch::Spans(rows) => insert_spans(state, source, &rows[..granted]).await,
    };
    if let Err(failure) = written {
        let released = sqlx::query(
            "UPDATE ingest_usage SET count = count - $1 WHERE kind = $2 AND subject_id = $3 AND day = $4",
        )
        .bind(granted as i64)
        .bind(kind)
        .bind(&source.id)
        .bind(&day)
        .execute(state.db.pool())
        .await;
        if let Err(error) = released {
            tracing::warn!(%error, kind, "failed to release quota");
        }
        return Err(failure);
    }
    Ok(granted)
}

/// One transaction: every row and the source's first log time.
async fn insert_logs(
    state: &AppState,
    source: &Source,
    rows: &[LogRow],
) -> Result<(), sqlx::Error> {
    let mut tx = state.db.begin_immediate().await?;
    let ids: Vec<String> = rows
        .iter()
        .map(|_| uuid::Uuid::now_v7().to_string())
        .collect();
    for (chunk, chunk_ids) in rows.chunks(INSERT_ROWS).zip(ids.chunks(INSERT_ROWS)) {
        let sql = format!(
            "INSERT INTO logs (id, source_id, time_ms, level, severity_text, service, environment, host, \
             body, trace_id, span_id, attributes_json, resource_json) VALUES {}",
            placeholders(state.db.backend(), chunk.len(), LOG_COLUMNS)
        );
        let mut insert = sqlx::query(&sql);
        for (row, id) in chunk.iter().zip(chunk_ids) {
            insert = insert
                .bind(id)
                .bind(&source.id)
                .bind(row.time_ms)
                .bind(row.level.as_str())
                .bind(&row.severity_text)
                .bind(&row.service)
                .bind(&row.environment)
                .bind(&row.host)
                .bind(&row.body)
                .bind(&row.trace_id)
                .bind(&row.span_id)
                .bind(&row.attributes_json)
                .bind(&row.resource_json);
        }
        insert.execute(&mut *tx).await?;
    }
    let pairs: Vec<(&str, &str, &str)> = rows
        .iter()
        .zip(&ids)
        .flat_map(|(row, id)| {
            row.attributes
                .iter()
                .map(move |(key, value)| (id.as_str(), key.as_str(), value.as_str()))
        })
        .collect();
    for chunk in pairs.chunks(INSERT_ROWS) {
        let sql = format!(
            "INSERT INTO log_attributes (log_id, key, value) VALUES {}",
            placeholders(state.db.backend(), chunk.len(), 3)
        );
        let mut insert = sqlx::query(&sql);
        for (id, key, value) in chunk {
            insert = insert.bind(*id).bind(*key).bind(*value);
        }
        insert.execute(&mut *tx).await?;
    }
    if source.first_log_at.is_none() {
        sqlx::query(
            "UPDATE telemetry_sources SET first_log_at = $1 WHERE id = $2 AND first_log_at IS NULL",
        )
        .bind(TimestampMillis::now().as_millis())
        .bind(&source.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// One transaction: the spans, their attributes, and the rollup. A span that is already
/// stored, from a retry, is skipped by the insert and is not counted again in the rollup.
async fn insert_spans(
    state: &AppState,
    source: &Source,
    rows: &[SpanRow],
) -> Result<(), sqlx::Error> {
    let backend = state.db.backend();
    let mut tx = state.db.begin_immediate().await?;
    let mut inserted: Vec<&SpanRow> = Vec::new();
    for chunk in rows.chunks(INSERT_ROWS) {
        let sql = format!(
            "INSERT INTO spans (source_id, trace_id, span_id, parent_span_id, service, environment, \
             host, name, kind, start_us, duration_us, status, status_message, is_entry, http_method, \
             http_route, http_status, attributes_json, events_json, links_json, resource_json) \
             VALUES {} ON CONFLICT DO NOTHING RETURNING trace_id, span_id",
            placeholders(backend, chunk.len(), SPAN_COLUMNS)
        );
        let mut insert = sqlx::query(&sql);
        for row in chunk {
            insert = insert
                .bind(&source.id)
                .bind(&row.trace_id)
                .bind(&row.span_id)
                .bind(&row.parent_span_id)
                .bind(&row.service)
                .bind(&row.environment)
                .bind(&row.host)
                .bind(&row.name)
                .bind(row.kind.as_str())
                .bind(row.start_us)
                .bind(row.duration_us)
                .bind(row.status.as_str())
                .bind(&row.status_message)
                .bind(i64::from(row.is_entry))
                .bind(&row.http_method)
                .bind(&row.http_route)
                .bind(row.http_status)
                .bind(&row.attributes_json)
                .bind(&row.events_json)
                .bind(&row.links_json)
                .bind(&row.resource_json);
        }
        let mut new: HashSet<(String, String)> = insert
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(|row| Ok((row.try_get("trace_id")?, row.try_get("span_id")?)))
            .collect::<Result<_, sqlx::Error>>()?;
        // `remove` takes a span sent two times in one chunk only once.
        inserted.extend(
            chunk
                .iter()
                .filter(|row| new.remove(&(row.trace_id.clone(), row.span_id.clone()))),
        );
    }
    let pairs: Vec<(&SpanRow, &str, &str)> = inserted
        .iter()
        .flat_map(|row| {
            row.attributes
                .iter()
                .map(move |(key, value)| (*row, key.as_str(), value.as_str()))
        })
        .collect();
    for chunk in pairs.chunks(INSERT_ROWS) {
        let sql = format!(
            "INSERT INTO span_attributes (source_id, trace_id, span_id, key, value) VALUES {}",
            placeholders(backend, chunk.len(), 5)
        );
        let mut insert = sqlx::query(&sql);
        for (row, key, value) in chunk {
            insert = insert
                .bind(&source.id)
                .bind(&row.trace_id)
                .bind(&row.span_id)
                .bind(*key)
                .bind(*value);
        }
        insert.execute(&mut *tx).await?;
    }
    let entries: Vec<&SpanRow> = inserted.into_iter().filter(|row| row.is_entry).collect();
    if !entries.is_empty() {
        rollup(&mut tx, backend, &source.id, &entries).await?;
    }
    tx.commit().await
}

/// One rollup row: entry spans of one service, operation, and minute.
#[derive(Default)]
struct Tally {
    count: i64,
    errors: i64,
    duration_sum_us: i64,
    buckets: [i64; BUCKETS],
}

/// Adds the entry spans to their operation's row and to the service total (operation `''`).
async fn rollup(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    backend: Backend,
    source_id: &str,
    entries: &[&SpanRow],
) -> Result<(), sqlx::Error> {
    // Sorted, so parallel requests take the row locks in the same order.
    let wanted: BTreeSet<(&str, String)> = entries
        .iter()
        .map(|row| (row.service.as_str(), row.operation()))
        .collect();
    let mut known: HashMap<&str, HashSet<String>> = HashMap::new();
    let mut operations: HashMap<(&str, String), String> = HashMap::new();
    for (service, operation) in wanted {
        let names = match known.entry(service) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let rows = sqlx::query(
                    "SELECT operation FROM span_operations WHERE source_id = $1 AND service = $2",
                )
                .bind(source_id)
                .bind(service)
                .fetch_all(&mut **tx)
                .await?;
                entry.insert(
                    rows.iter()
                        .map(|row| row.try_get("operation"))
                        .collect::<Result<_, _>>()?,
                )
            }
        };
        let stored = if names.contains(&operation) {
            operation.clone()
        } else if names.len() < MAX_OPERATIONS {
            sqlx::query(
                "INSERT INTO span_operations (source_id, service, operation) VALUES ($1, $2, $3) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(source_id)
            .bind(service)
            .bind(&operation)
            .execute(&mut **tx)
            .await?;
            names.insert(operation.clone());
            operation.clone()
        } else {
            OTHER_OPERATION.to_owned()
        };
        operations.insert((service, operation), stored);
    }
    let mut tallies: BTreeMap<(&str, String, i64), Tally> = BTreeMap::new();
    for row in entries {
        let operation = operations[&(row.service.as_str(), row.operation())].clone();
        let minute = row.start_us.div_euclid(60_000_000);
        for operation in [operation, String::new()] {
            let tally = tallies
                .entry((row.service.as_str(), operation, minute))
                .or_default();
            tally.count += 1;
            tally.errors += i64::from(row.status == SpanStatus::Error);
            tally.duration_sum_us = tally.duration_sum_us.saturating_add(row.duration_us);
            tally.buckets[bucket_index(row.duration_us)] += 1;
        }
    }
    let buckets: Vec<String> = (0..BUCKETS).map(|index| format!("b{index:02}")).collect();
    let updates: Vec<String> = ["count", "errors", "duration_sum_us"]
        .into_iter()
        .map(str::to_owned)
        .chain(buckets.iter().cloned())
        .map(|column| format!("{column} = span_rollups.{column} + excluded.{column}"))
        .collect();
    let tallies: Vec<_> = tallies.into_iter().collect();
    for chunk in tallies.chunks(INSERT_ROWS) {
        let sql = format!(
            "INSERT INTO span_rollups (source_id, service, operation, minute, count, errors, \
             duration_sum_us, {}) VALUES {} \
             ON CONFLICT (source_id, service, operation, minute) DO UPDATE SET {}",
            buckets.join(", "),
            placeholders(backend, chunk.len(), 7 + BUCKETS),
            updates.join(", ")
        );
        let mut upsert = sqlx::query(&sql);
        for ((service, operation, minute), tally) in chunk {
            upsert = upsert
                .bind(source_id)
                .bind(*service)
                .bind(operation)
                .bind(*minute)
                .bind(tally.count)
                .bind(tally.errors)
                .bind(tally.duration_sum_us);
            for count in tally.buckets {
                upsert = upsert.bind(count);
            }
        }
        upsert.execute(&mut **tx).await?;
    }
    Ok(())
}

/// Adds up to `wanted` items to the day's count without passing `quota`, and returns how many
/// were added. It is a short transaction of its own: the first statement locks the counter row,
/// so parallel requests cannot pass the cap together, and the row writes do not hold that lock.
async fn reserve(
    state: &AppState,
    kind: &str,
    source_id: &str,
    day: &str,
    wanted: usize,
    quota: i64,
) -> Result<usize, sqlx::Error> {
    let mut tx = state.db.begin_immediate().await?;
    let used: i64 = sqlx::query(
        "INSERT INTO ingest_usage (kind, subject_id, day, count) VALUES ($1, $2, $3, 0) \
         ON CONFLICT (kind, subject_id, day) DO UPDATE SET count = ingest_usage.count \
         RETURNING count",
    )
    .bind(kind)
    .bind(source_id)
    .bind(day)
    .fetch_one(&mut *tx)
    .await?
    .try_get("count")?;
    let room = usize::try_from(quota.saturating_sub(used)).unwrap_or(0);
    let granted = wanted.min(room);
    if granted > 0 {
        sqlx::query(
            "UPDATE ingest_usage SET count = count + $1 WHERE kind = $2 AND subject_id = $3 AND day = $4",
        )
        .bind(granted as i64)
        .bind(kind)
        .bind(source_id)
        .bind(day)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(granted)
}

/// `($1, $2), ($3, $4)` for two rows of two columns on Postgres, `(?, ?), (?, ?)` on SQLite.
/// Numbered parameters cost SQLite time that grows faster than the row count:
/// 500-row statements stored 12,000 rows per second with them and 48,000 without.
fn placeholders(backend: Backend, rows: usize, columns: usize) -> String {
    let mut sql = String::with_capacity(rows * columns * 5);
    for row in 0..rows {
        if row > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for column in 0..columns {
            if column > 0 {
                sql.push_str(", ");
            }
            match backend {
                Backend::Sqlite => sql.push('?'),
                Backend::Postgres => {
                    sql.push('$');
                    sql.push_str(&(row * columns + column + 1).to_string());
                }
            }
        }
        sql.push(')');
    }
    sql
}

type Refusal = fn(Encoding) -> Response;

/// Reads at most `OWLEE_OTLP_MAX_BODY_BYTES`, before and after gzip.
async fn read_body(
    state: &AppState,
    headers: &HeaderMap,
    request: Request,
) -> Result<Vec<u8>, Refusal> {
    let max = state.config.ingest.otlp_max_body_bytes;
    let declared =
        header_str(headers, header::CONTENT_LENGTH).and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > max) {
        return Err(too_large);
    }
    let bytes = match axum::body::to_bytes(request.into_body(), max).await {
        Ok(bytes) => bytes,
        Err(read) => {
            let over = std::error::Error::source(&read)
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>());
            return Err(if over { too_large } else { unreadable });
        }
    };
    decode_body(header_str(headers, header::CONTENT_ENCODING), &bytes, max).map_err(|decode| {
        tracing::warn!(error = %decode, "rejected OTLP body");
        match decode {
            BodyError::TooLarge => too_large as Refusal,
            _ => unreadable,
        }
    })
}

fn too_large(encoding: Encoding) -> Response {
    error(
        encoding,
        StatusCode::PAYLOAD_TOO_LARGE,
        "body is above the size limit",
    )
}

fn unreadable(encoding: Encoding) -> Response {
    error(
        encoding,
        StatusCode::BAD_REQUEST,
        "body could not be read or decompressed",
    )
}

/// `Authorization: Bearer <token>`, as `OTEL_EXPORTER_OTLP_HEADERS` sends it.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let (scheme, token) = header_str(headers, header::AUTHORIZATION)?.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

fn ok(encoding: Encoding, body: Vec<u8>) -> Response {
    respond(encoding, StatusCode::OK, body)
}

/// gRPC status codes in the body, as the OTLP specification asks.
fn error(encoding: Encoding, status: StatusCode, message: &str) -> Response {
    let code = match status {
        StatusCode::UNAUTHORIZED => 16,
        StatusCode::TOO_MANY_REQUESTS => 8,
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::INTERNAL_SERVER_ERROR => 14,
        _ => 3,
    };
    respond(encoding, status, status_body(encoding, code, message))
}

fn limited(encoding: Encoding, retry_after_secs: u64, message: &str) -> Response {
    let mut response = error(encoding, StatusCode::TOO_MANY_REQUESTS, message);
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.max(1).to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

fn respond(encoding: Encoding, status: StatusCode, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(encoding.content_type()),
    );
    response
}
