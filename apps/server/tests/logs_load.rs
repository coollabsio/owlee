//! Load test for logs (observability design §10, P0.4). Not part of the normal test run.
//!
//! Seed rows straight into a database, for one new source:
//!
//! ```sh
//! LOAD_DATABASE=/tmp/load.sqlite LOAD_ROWS=1000000 \
//!   cargo test --release -p owlee-server --test logs_load -- --ignored --nocapture seed
//! ```
//!
//! Time searches over 15 minutes, 24 hours, and 7 days (`LOAD_SOURCE` defaults to the newest seeded source):
//!
//! ```sh
//! LOAD_DATABASE=/tmp/load.sqlite \
//!   cargo test --release -p owlee-server --test logs_load -- --ignored --nocapture search
//! ```
//!
//! Send OTLP protobuf requests at a fixed rate. Without `LOAD_URL` the test serves a new database itself:
//!
//! ```sh
//! LOAD_URL=http://127.0.0.1:43124 LOAD_TOKEN=otlp_... LOAD_RATE=50 LOAD_BATCH=100 LOAD_SECS=60 \
//!   cargo test --release -p owlee-server --test logs_load -- --ignored --nocapture send
//! ```
//!
//! `LOAD_DATABASE` is a SQLite path or a `postgres://` URL.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_platform::{Backend, Config};
use owlee_server::{AppState, boot, editor_session, member_session, router, testing_config};
use owlee_telemetry::prost::Message;
use owlee_telemetry::proto::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use owlee_telemetry::proto::common::v1::any_value::Value as Any;
use owlee_telemetry::proto::common::v1::{AnyValue, KeyValue};
use owlee_telemetry::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use owlee_telemetry::proto::resource::v1::Resource;
use serde_json::{Value, json};
use sqlx::Row;
use tokio::sync::Mutex;
use tower::ServiceExt;

const DAY_MS: i64 = 86_400_000;
const SOURCE_PREFIX: &str = "load-test-";
const SERVICES: [&str; 10] = [
    "checkout", "api", "worker", "auth", "search", "billing", "mailer", "gateway", "cart", "media",
];
const ROUTES: [&str; 8] = [
    "/checkout",
    "/cart",
    "/login",
    "/search",
    "/products",
    "/orders",
    "/account",
    "/health",
];
const BODIES: [&str; 8] = [
    "request completed",
    "cache miss for product",
    "user signed in",
    "payment authorized",
    "slow query detected",
    "upstream timeout after 30s",
    "retrying job",
    "connection reset by peer",
];

/// Deterministic xorshift, so every run seeds the same data.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// 70% info, 15% debug, 8% warn, 6% error, 1% fatal.
fn level(rng: &mut Rng) -> &'static str {
    match rng.below(100) {
        0..70 => "info",
        70..85 => "debug",
        85..93 => "warn",
        93..99 => "error",
        _ => "fatal",
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.replace('_', "").parse().ok())
        .unwrap_or(default)
}

fn config(database: &str) -> Config {
    if database.starts_with("postgres") {
        let mut config = testing_config("/tmp/unused-owlee.sqlite".into());
        config.database_url = Some(database.to_owned());
        config
    } else {
        testing_config(database.into())
    }
}

fn temp_database() -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("load.sqlite").display().to_string();
    let _keep = Box::leak(Box::new(dir));
    path
}

async fn call(
    app: &axum::Router,
    cookie: &str,
    workspace: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header("x-owlee-workspace", workspace)
        .header("content-type", "application/json")
        .header("origin", "http://127.0.0.1")
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            1,
        ))))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A new workspace with one source. Returns the source id and its token.
async fn new_source(state: &AppState) -> (String, String) {
    let editor = editor_session(state).await.unwrap();
    let name = format!("{SOURCE_PREFIX}{}", chrono::Utc::now().timestamp_millis());
    let (status, body) = call(
        &router(state.clone()),
        &editor.cookie,
        &editor.workspace_id.to_string(),
        "POST",
        "/api/telemetry-sources",
        json!({ "project_id": editor.project_id, "name": name }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["id"].as_str().unwrap().to_owned(),
        body["token"].as_str().unwrap().to_owned(),
    )
}

/// Positional `?` on SQLite: numbered parameters make large statements several times slower there.
fn placeholders(backend: Backend, rows: usize, columns: usize) -> String {
    (0..rows)
        .map(|row| {
            let cells: Vec<String> = (1..=columns)
                .map(|column| match backend {
                    Backend::Sqlite => "?".to_owned(),
                    Backend::Postgres => format!("${}", row * columns + column),
                })
                .collect();
            format!("({})", cells.join(", "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[tokio::test]
#[ignore = "load test; see the module docs"]
async fn seed() {
    let database = env_or("LOAD_DATABASE", "");
    assert!(
        !database.is_empty(),
        "set LOAD_DATABASE to a SQLite path or a postgres:// URL"
    );
    let rows = number("LOAD_ROWS", 10_000) as usize;
    let state = boot(config(&database)).await.expect("boot");
    let backend = state.db.backend();
    let (source_id, token) = new_source(&state).await;
    println!("LOAD_SOURCE={source_id}");
    println!("LOAD_TOKEN={token}");
    let mut rng = Rng(0x5eed);
    let now = chrono::Utc::now().timestamp_millis();
    let started = Instant::now();
    let per_statement = 500;
    let per_transaction = 20_000;
    let log_sql = format!(
        "INSERT INTO logs (id, source_id, time_ms, level, service, host, body, attributes_json, resource_json) VALUES {}",
        placeholders(backend, per_statement, 9)
    );
    let attribute_sql = format!(
        "INSERT INTO log_attributes (log_id, key, value) VALUES {}",
        placeholders(backend, per_statement, 3)
    );
    let mut done = 0;
    while done < rows {
        let mut tx = state.db.begin().await.unwrap();
        let end = (done + per_transaction).min(rows);
        while done < end {
            let count = per_statement.min(end - done);
            let mut logs = Vec::with_capacity(count);
            for _ in 0..count {
                let service = SERVICES[rng.below(10) as usize];
                let route = ROUTES[rng.below(8) as usize];
                let status = if rng.below(20) == 0 { "500" } else { "200" };
                let tier = if rng.below(4) == 0 { "pro" } else { "free" };
                logs.push((
                    uuid::Uuid::now_v7().to_string(),
                    now - rng.below(7 * DAY_MS as u64) as i64,
                    level(&mut rng),
                    service,
                    BODIES[rng.below(8) as usize],
                    [
                        ("http.route", route),
                        ("http.status_code", status),
                        ("user.tier", tier),
                    ],
                ));
            }
            let full = count == per_statement;
            let sql = if full {
                log_sql.clone()
            } else {
                format!(
                    "INSERT INTO logs (id, source_id, time_ms, level, service, host, body, attributes_json, resource_json) VALUES {}",
                    placeholders(backend, count, 9)
                )
            };
            let mut insert = sqlx::query(&sql);
            for (id, time_ms, level, service, body, attributes) in &logs {
                let attributes_json = json!({ "http.route": attributes[0].1, "http.status_code": attributes[1].1, "user.tier": attributes[2].1 });
                insert = insert
                    .bind(id.clone())
                    .bind(source_id.clone())
                    .bind(*time_ms)
                    .bind(*level)
                    .bind(*service)
                    .bind("load-host")
                    .bind(*body)
                    .bind(attributes_json.to_string())
                    .bind(json!({ "service.name": service }).to_string());
            }
            insert.execute(&mut *tx).await.unwrap();
            // Three attributes per row: one INSERT of 500 attribute rows per 166 logs.
            let pairs: Vec<(&String, &str, &str)> = logs
                .iter()
                .flat_map(|(id, _, _, _, _, attributes)| {
                    attributes
                        .iter()
                        .map(move |(key, value)| (id, *key, *value))
                })
                .collect();
            for chunk in pairs.chunks(per_statement) {
                let sql = if chunk.len() == per_statement {
                    attribute_sql.clone()
                } else {
                    format!(
                        "INSERT INTO log_attributes (log_id, key, value) VALUES {}",
                        placeholders(backend, chunk.len(), 3)
                    )
                };
                let mut insert = sqlx::query(&sql);
                for (id, key, value) in chunk {
                    insert = insert.bind((*id).clone()).bind(*key).bind(*value);
                }
                insert.execute(&mut *tx).await.unwrap();
            }
            done += count;
        }
        tx.commit().await.unwrap();
        if done % 1_000_000 < per_transaction {
            println!("seeded {done} rows in {:.1?}", started.elapsed());
        }
    }
    let secs = started.elapsed().as_secs_f64();
    println!(
        "seeded {rows} rows in {secs:.1}s ({:.0} rows/s) into source {source_id}",
        rows as f64 / secs
    );
}

#[tokio::test]
#[ignore = "load test; see the module docs"]
async fn search() {
    let database = env_or("LOAD_DATABASE", "");
    assert!(
        !database.is_empty(),
        "set LOAD_DATABASE to a SQLite path or a postgres:// URL"
    );
    let runs = number("LOAD_RUNS", 3);
    let state = boot(config(&database)).await.expect("boot");
    let source = match std::env::var("LOAD_SOURCE") {
        Ok(id) => sqlx::query("SELECT s.id, p.workspace_id FROM telemetry_sources s JOIN projects p ON p.id = s.project_id WHERE s.id = $1")
            .bind(id)
            .fetch_one(state.db.pool())
            .await
            .expect("LOAD_SOURCE not found"),
        Err(_) => sqlx::query("SELECT s.id, p.workspace_id FROM telemetry_sources s JOIN projects p ON p.id = s.project_id WHERE s.name LIKE $1 ORDER BY s.created_at DESC LIMIT 1")
            .bind(format!("{SOURCE_PREFIX}%"))
            .fetch_one(state.db.pool())
            .await
            .expect("no seeded source; run the seed test first"),
    };
    let source_id: String = source.try_get("id").unwrap();
    let workspace: String = source.try_get("workspace_id").unwrap();
    let rows: i64 = sqlx::query("SELECT COUNT(*) FROM logs WHERE source_id = $1")
        .bind(&source_id)
        .fetch_one(state.db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    println!("source {source_id}: {rows} rows; median of {runs} runs");
    let member = member_session(&state, workspace.parse().unwrap())
        .await
        .unwrap();
    let app = router(state.clone());
    let now = chrono::Utc::now().timestamp_millis();
    for (window, span) in [("15m", 15 * 60_000), ("24h", DAY_MS), ("7d", 7 * DAY_MS)] {
        for (name, q) in [
            ("range", ""),
            ("service", "service:checkout"),
            ("level>=warn", "level:>=warn"),
            ("text", "timeout"),
            ("attribute", "http.route:/checkout"),
        ] {
            for path in ["/api/logs", "/api/logs/facets"] {
                let uri = format!(
                    "{path}?from={}&to={now}&q={}",
                    now - span,
                    percent_encoding::utf8_percent_encode(q, percent_encoding::NON_ALPHANUMERIC)
                );
                let mut times = Vec::new();
                let mut found = 0;
                for _ in 0..runs {
                    let started = Instant::now();
                    let (status, body) =
                        call(&app, &member.cookie, &workspace, "GET", &uri, Value::Null).await;
                    times.push(started.elapsed());
                    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
                    found = body["items"].as_array().map_or(0, Vec::len);
                }
                times.sort();
                println!(
                    "{window:>4} {name:<12} {path:<17} {:>9.1} ms{}",
                    times[times.len() / 2].as_secs_f64() * 1000.0,
                    if path == "/api/logs" {
                        format!("  ({found} rows on page)")
                    } else {
                        String::new()
                    }
                );
            }
        }
    }
}

fn request_body(rng: &mut Rng, batch: usize) -> Vec<u8> {
    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
    let text = |value: &str| {
        Some(AnyValue {
            value: Some(Any::StringValue(value.to_owned())),
        })
    };
    let pair = |key: &str, value: &str| KeyValue {
        key: key.to_owned(),
        value: text(value),
        ..KeyValue::default()
    };
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![
                    pair("service.name", SERVICES[rng.below(10) as usize]),
                    pair("host.name", "load-host"),
                ],
                ..Resource::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: (0..batch)
                    .map(|_| LogRecord {
                        time_unix_nano: now_ns,
                        severity_number: [9, 9, 9, 13, 17][rng.below(5) as usize],
                        body: text(BODIES[rng.below(8) as usize]),
                        attributes: vec![
                            pair("http.route", ROUTES[rng.below(8) as usize]),
                            pair("http.status_code", "200"),
                            pair("user.tier", "free"),
                        ],
                        ..LogRecord::default()
                    })
                    .collect(),
                ..ScopeLogs::default()
            }],
            ..ResourceLogs::default()
        }],
    }
    .encode_to_vec()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; see the module docs"]
async fn send() {
    let rate = number("LOAD_RATE", 10).max(1);
    let batch = number("LOAD_BATCH", 100) as usize;
    let secs = number("LOAD_SECS", 10);
    let (url, token) = match std::env::var("LOAD_URL") {
        Ok(url) => (
            url,
            std::env::var("LOAD_TOKEN").expect("set LOAD_TOKEN with LOAD_URL"),
        ),
        Err(_) => {
            let database = env_or("LOAD_DATABASE", &temp_database());
            let mut config = config(&database);
            // A fixed-rate test measures storage, not the guards.
            config.ingest.otlp_rate_source_per_min = 1_000_000;
            config.ingest.otlp_rate_source_burst = 100_000;
            config.ingest.otlp_rate_ip_per_min = 1_000_000;
            config.ingest.otlp_rate_ip_burst = 100_000;
            config.ingest.log_daily_quota = i64::MAX;
            let state = boot(config).await.expect("boot");
            let (_, token) = new_source(&state).await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = router(state);
            tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .unwrap();
            });
            println!("serving {database} at {address}");
            (format!("http://{address}"), token)
        }
    };
    let client = reqwest::Client::new();
    let endpoint = format!("{}/otlp/v1/logs", url.trim_end_matches('/'));
    let results: Arc<Mutex<Vec<(Duration, u16, u64)>>> = Arc::default();
    let mut ticker = tokio::time::interval(Duration::from_secs_f64(1.0 / rate as f64));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut rng = Rng(0xfeed);
    let started = Instant::now();
    let mut tasks = Vec::new();
    while started.elapsed() < Duration::from_secs(secs) {
        ticker.tick().await;
        let body = request_body(&mut rng, batch);
        let (client, endpoint, token, results) = (
            client.clone(),
            endpoint.clone(),
            token.clone(),
            Arc::clone(&results),
        );
        tasks.push(tokio::spawn(async move {
            let sent = Instant::now();
            let response = client
                .post(&endpoint)
                .bearer_auth(&token)
                .header("content-type", "application/x-protobuf")
                .body(body)
                .send()
                .await;
            let (status, rejected) = match response {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let bytes = response.bytes().await.unwrap_or_default();
                    let rejected = ExportLogsServiceResponse::decode(bytes.as_ref())
                        .ok()
                        .and_then(|decoded| decoded.partial_success)
                        .map_or(0, |partial| partial.rejected_log_records as u64);
                    (status, rejected)
                }
                Err(_) => (0, 0),
            };
            results
                .lock()
                .await
                .push((sent.elapsed(), status, rejected));
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let wall = started.elapsed().as_secs_f64();
    let mut results = results.lock().await.clone();
    results.sort_by_key(|(elapsed, _, _)| *elapsed);
    let ok: Vec<_> = results
        .iter()
        .filter(|(_, status, _)| *status == 200)
        .collect();
    let stored: u64 = ok
        .iter()
        .map(|(_, _, rejected)| batch as u64 - rejected)
        .sum();
    let percentile = |p: f64| {
        results
            .get(((results.len() as f64 * p) as usize).min(results.len().saturating_sub(1)))
            .map_or(0.0, |(elapsed, _, _)| elapsed.as_secs_f64() * 1000.0)
    };
    println!(
        "{} requests at {rate}/s of {batch} records for {secs}s: {} ok, {} failed; stored {stored} rows, {:.0} rows/s; p50 {:.1} ms, p95 {:.1} ms",
        results.len(),
        ok.len(),
        results.len() - ok.len(),
        stored as f64 / wall,
        percentile(0.50),
        percentile(0.95)
    );
}
