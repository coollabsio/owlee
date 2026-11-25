//! OTLP/HTTP intake: `POST /otlp/v1/{logs,traces,metrics}` (D6). Logs are
//! stored. Traces and metrics answer 200 with every item rejected in
//! `partial_success` until their phase.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::response::Response;
use owlee_errors::{BodyError, decode_body};
use owlee_platform::{Backend, TimestampMillis};
use owlee_telemetry::{
    Encoding, LogRow, Signal, count_points, count_spans, decode_logs, export_response,
    normalize_logs, status_body,
};

use super::sources::{self, Source, intake_key};
use crate::intake::DropReason;
use crate::peer::Client;
use crate::state::AppState;

/// Rows of one multi-row `INSERT`.
const INSERT_ROWS: usize = 500;
const LOG_COLUMNS: usize = 13;
const QUOTA_KIND: &str = "log";

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
    match signal {
        Signal::Logs => store_logs(state, &source, &key, &body, encoding).await,
        Signal::Traces => reject_all(state, &key, count_spans(&body, encoding), encoding, signal),
        Signal::Metrics => reject_all(state, &key, count_points(&body, encoding), encoding, signal),
    }
}

fn reject_all(
    state: &AppState,
    key: &str,
    counted: Result<u64, owlee_telemetry::DecodeError>,
    encoding: Encoding,
    signal: Signal,
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
    let message = match signal {
        Signal::Traces => "traces are not stored yet",
        _ => "metrics are not stored yet",
    };
    ok(encoding, export_response(signal, encoding, count, message))
}

async fn store_logs(
    state: &AppState,
    source: &Source,
    key: &str,
    body: &[u8],
    encoding: Encoding,
) -> Response {
    let request = match decode_logs(body, encoding) {
        Ok(request) => request,
        Err(decode) => {
            state.telemetry_intake.dropped(key, DropReason::Malformed);
            return error(encoding, StatusCode::BAD_REQUEST, &decode.to_string());
        }
    };
    let rows = normalize_logs(&request, TimestampMillis::now().as_millis());
    if rows.is_empty() {
        return ok(encoding, export_response(Signal::Logs, encoding, 0, ""));
    }
    let quota = source
        .daily_quota
        .unwrap_or(state.config.ingest.log_daily_quota);
    let stored = match write_logs(state, source, &rows, quota).await {
        Ok(stored) => stored,
        Err(failure) => {
            state
                .telemetry_intake
                .dropped_many(key, rows.len() as u64, DropReason::StorageFailed);
            tracing::error!(error = %failure, "failed to store logs");
            return error(
                encoding,
                StatusCode::SERVICE_UNAVAILABLE,
                "failed to store logs",
            );
        }
    };
    let rejected = (rows.len() - stored) as u64;
    if stored > 0 {
        state.telemetry_intake.stored_many(key, stored as u64);
    }
    if rejected > 0 {
        state
            .telemetry_intake
            .dropped_many(key, rejected, DropReason::DailyQuota);
    }
    if stored == 0 {
        return limited(
            encoding,
            crate::quota::secs_until_utc_midnight(),
            "daily log quota reached",
        );
    }
    ok(
        encoding,
        export_response(Signal::Logs, encoding, rejected, "daily log quota reached"),
    )
}

/// Reserves quota, then stores the granted rows. Returns how many of `rows` were stored;
/// the rest are over the quota. A failed write gives its reservation back.
async fn write_logs(
    state: &AppState,
    source: &Source,
    rows: &[LogRow],
    quota: i64,
) -> Result<usize, sqlx::Error> {
    let day = crate::quota::today();
    let granted = reserve(state, &source.id, &day, rows.len(), quota).await?;
    if granted == 0 {
        return Ok(0);
    }
    if let Err(failure) = insert_logs(state, source, &rows[..granted]).await {
        let released = sqlx::query(
            "UPDATE ingest_usage SET count = count - $1 WHERE kind = $2 AND subject_id = $3 AND day = $4",
        )
        .bind(granted as i64)
        .bind(QUOTA_KIND)
        .bind(&source.id)
        .bind(&day)
        .execute(state.db.pool())
        .await;
        if let Err(error) = released {
            tracing::warn!(%error, "failed to release log quota");
        }
        return Err(failure);
    }
    Ok(granted)
}

/// One transaction: every row and the source's first log time.
async fn insert_logs(state: &AppState, source: &Source, rows: &[LogRow]) -> Result<(), sqlx::Error> {
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

/// Adds up to `wanted` items to the day's count without passing `quota`, and returns how many
/// were added. It is a short transaction of its own: the first statement locks the counter row,
/// so parallel requests cannot pass the cap together, and the row writes do not hold that lock.
async fn reserve(
    state: &AppState,
    source_id: &str,
    day: &str,
    wanted: usize,
    quota: i64,
) -> Result<usize, sqlx::Error> {
    use sqlx::Row;
    let mut tx = state.db.begin_immediate().await?;
    let used: i64 = sqlx::query(
        "INSERT INTO ingest_usage (kind, subject_id, day, count) VALUES ($1, $2, $3, 0) \
         ON CONFLICT (kind, subject_id, day) DO UPDATE SET count = ingest_usage.count \
         RETURNING count",
    )
    .bind(QUOTA_KIND)
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
        .bind(QUOTA_KIND)
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
