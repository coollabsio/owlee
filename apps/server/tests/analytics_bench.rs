//! Analytics write and read benchmark on the real handlers. Not part of the normal test run:
//!
//! ```sh
//! cargo test --release -p owlee-server --test analytics_bench -- --ignored --nocapture
//! BENCH_POSTGRES_URL=postgres://... cargo test --release -p owlee-server --test analytics_bench -- --ignored --nocapture
//! ```
//!
//! `BENCH_EVENTS` rows are loaded in bulk (default 5,000,000, over 90 days). Then `BENCH_INGEST` events
//! (default 20,000) go through `POST /api/receive`, `BENCH_CONCURRENCY` at a time (default 32).
//! Then every dashboard request is timed for 7, 30, and 90 days.

use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use owlee_server::{AppState, TestSession, boot, editor_session, router, testing_config};
use serde_json::{Value, json};
use tower::ServiceExt;

const DAY: i64 = 86_400;
const DAYS: i64 = 90;
const DOMAIN: &str = "bench.test";

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// Skewed pick: low indexes are much more common, as with real pages and referrers.
    fn skewed(&mut self, n: u64) -> u64 {
        let r = (self.next() >> 11) as f64 / (1_u64 << 53) as f64;
        ((r * r * r) * n as f64) as u64
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    session: Option<&TestSession>,
    user_agent: &str,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("user-agent", user_agent);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if uri == "/api/receive" {
        request = request.header("origin", format!("https://{DOMAIN}"));
    }
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-owlee-workspace", session.workspace_id.to_string());
    }
    let body = Body::from(body.map(|v| v.to_string()).unwrap_or_default());
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn user_agent(visitor: u64) -> String {
    format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 Chrome/128.0.{visitor}.0 Safari/537.36"
    )
}

const BROWSERS: [&str; 5] = ["Chrome", "Safari", "Firefox", "Edge", "Opera"];
const OSES: [&str; 5] = ["Windows", "macOS", "iOS", "Android", "Linux"];
const DEVICES: [&str; 3] = ["desktop", "mobile", "tablet"];
const COLUMNS: usize = 22;
const BATCH: usize = 400;
/// Rows written from Rust. The rest are SQL copies of them, because row-by-row loading of
/// millions of rows takes longer than the benchmark itself.
const BASE_ROWS: usize = 250_000;

/// Loads `total` rows over the last 90 days in big transactions, then builds `insight_daily`
/// from them, as ingest would have.
async fn bulk_load(
    state: &AppState,
    site_id: &str,
    total: usize,
    now: i64,
    postgres: bool,
) -> Duration {
    let started = Instant::now();
    let base = total.min(BASE_ROWS);
    write_base(state, site_id, base, now).await;
    let start = now - DAYS * DAY;
    let new_id = if postgres {
        "gen_random_uuid()::text"
    } else {
        "lower(hex(randomblob(16)))"
    };
    let mut copy = 1_i64;
    let mut written = base;
    while written < total {
        let take = (total - written).min(base);
        // A copy is the base shifted by a prime number of seconds, with new visitor and visit ids.
        let sql = format!(
            "INSERT INTO insight_events (id, site_id, timestamp, event_type, session_id, visit_id, pathname, hostname, \
             referrer_domain, utm_source, country, city, browser, os, device, screen, language, lcp, inp, cls, fcp, ttfb) \
             SELECT {new_id}, site_id, $2 + ((timestamp - $2 + $3) % $4), event_type, session_id || '-' || $5, \
             visit_id || '-' || $5, pathname, hostname, referrer_domain, utm_source, country, city, browser, os, device, \
             screen, language, lcp, inp, cls, fcp, ttfb \
             FROM insight_events WHERE site_id = $1 AND length(visit_id) = 32 LIMIT {take}"
        );
        sqlx::query(&sql)
            .bind(site_id)
            .bind(start)
            .bind(copy * 104_729)
            .bind(DAYS * DAY - 3_600)
            .bind(copy.to_string())
            .execute(state.db.pool())
            .await
            .unwrap();
        written += take;
        copy += 1;
    }
    sqlx::query(
        "INSERT INTO insight_daily (site_id, day, pageviews, visitors, events, visits, bounces, total_time) \
         SELECT site_id, timestamp - (timestamp % 86400), \
         SUM(CASE WHEN event_type = 'pageview' THEN 1 ELSE 0 END), COUNT(DISTINCT session_id), \
         SUM(CASE WHEN event_type NOT IN ('pageview', 'performance') THEN 1 ELSE 0 END), \
         COUNT(DISTINCT visit_id), 0, 0 \
         FROM insight_events WHERE site_id = $1 GROUP BY site_id, timestamp - (timestamp % 86400)",
    )
    .bind(site_id)
    .execute(state.db.pool())
    .await
    .unwrap();
    sqlx::query("ANALYZE")
        .execute(state.db.pool())
        .await
        .unwrap();
    started.elapsed()
}

async fn write_base(state: &AppState, site_id: &str, total: usize, now: i64) {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let visitors = (total / 12).max(1) as u64;
    let start = now - DAYS * DAY;
    let mut written = 0;
    let mut visit = 0_u64;
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(BATCH);
    let mut tx = state.db.begin().await.unwrap();
    let mut in_tx = 0;
    while written < total {
        visit += 1;
        let visitor = rng.below(visitors);
        let session = format!("{visitor:016x}");
        let visit_id = format!("{visit:016x}{visitor:016x}");
        let mut ts = start + rng.below((DAYS * DAY - 3_600) as u64) as i64;
        let country = format!("C{}", rng.skewed(60));
        let city = format!("City {}", rng.skewed(300));
        let browser = BROWSERS[rng.skewed(5) as usize];
        let os = OSES[rng.skewed(5) as usize];
        let device = DEVICES[rng.skewed(3) as usize];
        let referrer = match rng.skewed(51) {
            0 => None,
            n => Some(format!("ref{n}.example")),
        };
        let utm = (rng.below(10) == 0).then(|| "newsletter".to_owned());
        let pages = 1 + rng.skewed(6);
        for _ in 0..pages {
            ts += rng.below(120) as i64;
            let path = format!("/page/{}", rng.skewed(500));
            let push = |event_type: &str, vitals: bool, rows: &mut Vec<Vec<Value>>| {
                let v = |x: f64| if vitals { json!(x) } else { Value::Null };
                rows.push(vec![
                    json!(owlee_platform::Id::new().to_string()),
                    json!(site_id),
                    json!(ts),
                    json!(event_type),
                    json!(session),
                    json!(visit_id),
                    json!(path),
                    json!(DOMAIN),
                    json!(referrer),
                    json!(utm),
                    json!(country),
                    json!(city),
                    json!(browser),
                    json!(os),
                    json!(device),
                    json!("1920x1080"),
                    json!("en-US"),
                    v(1_800.0),
                    v(120.0),
                    v(0.05),
                    v(900.0),
                    v(200.0),
                ]);
            };
            push("pageview", false, &mut rows);
            if rng.below(100) < 15 {
                push("performance", true, &mut rows);
            }
            if rng.below(100) < 5 {
                push(
                    if rng.below(2) == 0 {
                        "signup"
                    } else {
                        "download"
                    },
                    false,
                    &mut rows,
                );
            }
        }
        if rows.len() >= BATCH || written + rows.len() >= total {
            rows.truncate(total - written);
            written += rows.len();
            insert_rows(&mut tx, &rows).await;
            rows.clear();
            in_tx += 1;
            if in_tx == 100 {
                tx.commit().await.unwrap();
                tx = state.db.begin().await.unwrap();
                in_tx = 0;
            }
        }
    }
    tx.commit().await.unwrap();
}

async fn insert_rows(tx: &mut sqlx::Transaction<'_, sqlx::Any>, rows: &[Vec<Value>]) {
    let mut sql = String::from(
        "INSERT INTO insight_events (id, site_id, timestamp, event_type, session_id, visit_id, pathname, hostname, \
         referrer_domain, utm_source, country, city, browser, os, device, screen, language, lcp, inp, cls, fcp, ttfb) VALUES ",
    );
    for row in 0..rows.len() {
        sql.push_str(if row == 0 { "(" } else { ",(" });
        for col in 0..COLUMNS {
            if col > 0 {
                sql.push(',');
            }
            sql.push_str(&format!("${}", row * COLUMNS + col + 1));
        }
        sql.push(')');
    }
    let mut query = sqlx::query(&sql);
    for row in rows {
        for (col, value) in row.iter().enumerate() {
            query = match (col, value) {
                (_, Value::Null) if col >= 17 => query.bind(owlee_platform::OptionalF64(None)),
                (_, Value::Null) => query.bind(None::<String>),
                (2, v) => query.bind(v.as_i64().unwrap()),
                (_, Value::Number(n)) => query.bind(owlee_platform::OptionalF64(n.as_f64())),
                (_, v) => query.bind(v.as_str().unwrap().to_owned()),
            };
        }
    }
    query.execute(&mut **tx).await.unwrap();
}

/// Sends `total` tracker events through the real handler, `concurrency` at a time.
async fn ingest(app: &Router, public_id: &str, total: usize, concurrency: usize) {
    let started = Instant::now();
    let results: Vec<(StatusCode, Duration)> = futures_util::stream::iter(0..total)
        .map(|i| {
            let app = app.clone();
            let public_id = public_id.to_owned();
            async move {
                let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (i as u64 + 1));
                let custom = rng.below(100) < 5;
                let body = json!({
                    "site": public_id,
                    "type": if custom { "signup" } else { "pageview" },
                    "url": format!("https://{DOMAIN}/page/{}?utm_source=x", rng.skewed(500)),
                    "referrer": format!("https://ref{}.example/", rng.skewed(50)),
                    "title": "Bench page",
                    "hostname": DOMAIN,
                    "screen": "1920x1080",
                    "data": if custom { json!({"plan": "pro", "seats": 3}) } else { Value::Null },
                });
                let ua = user_agent(rng.below(5_000));
                let t = Instant::now();
                let (status, _) = call(&app, "POST", "/api/receive", Some(body), None, &ua).await;
                (status, t.elapsed())
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;
    let wall = started.elapsed();
    let ok = results
        .iter()
        .filter(|(s, _)| *s == StatusCode::ACCEPTED)
        .count();
    let mut latencies: Vec<Duration> = results.iter().map(|(_, d)| *d).collect();
    latencies.sort();
    let pct = |p: f64| latencies[((latencies.len() - 1) as f64 * p) as usize];
    println!(
        "ingest: {total} events, {concurrency} at a time, {:.0} events/s, wall {:.2?}, p50 {:.2?}, p99 {:.2?}, accepted {ok}, other {}",
        total as f64 / wall.as_secs_f64(),
        wall,
        pct(0.5),
        pct(0.99),
        total - ok
    );
    let mut other: Vec<_> = results
        .iter()
        .filter(|(s, _)| *s != StatusCode::ACCEPTED)
        .map(|(s, _)| *s)
        .collect();
    other.dedup();
    assert_eq!(
        ok, total,
        "every event was accepted, other statuses: {other:?}"
    );
}

async fn timed(app: &Router, session: &TestSession, uri: &str) -> (Duration, Value) {
    let t = Instant::now();
    let (status, body) = call(app, "GET", uri, None, Some(session), "bench").await;
    let elapsed = t.elapsed();
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    (elapsed, body)
}

fn median(mut v: Vec<Duration>) -> (Duration, Duration, Duration) {
    v.sort();
    (v[v.len() / 2], v[0], v[v.len() - 1])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark: run with --ignored --release"]
async fn analytics_writes_and_dashboard_reads() {
    let events = env("BENCH_EVENTS", 5_000_000);
    let ingest_total = env("BENCH_INGEST", 20_000);
    let concurrency = env("BENCH_CONCURRENCY", 32);
    let runs = env("BENCH_RUNS", 5);

    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("analytics-bench");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = testing_config(dir.join("owlee.sqlite"));
    let backend = match std::env::var("BENCH_POSTGRES_URL") {
        Ok(url) => {
            config.database_url = Some(url);
            "postgres"
        }
        Err(_) => "sqlite",
    };
    // Rate limits off: this measures the database, not the token buckets.
    config.ingest.analytics_rate_site_per_min = u32::MAX;
    config.ingest.analytics_rate_site_burst = u32::MAX;
    config.ingest.analytics_rate_ip_per_min = u32::MAX;
    config.ingest.analytics_rate_ip_burst = u32::MAX;
    let _ = owlee_platform::init_tracing(owlee_platform::TracingFormat::Compact);
    let state = boot(config).await.unwrap();
    let app = router(state.clone());
    let session = editor_session(&state).await.unwrap();
    println!("backend: {backend}");

    let (status, site) = call(
        &app,
        "POST",
        "/api/sites",
        Some(json!({"project_id": session.project_id, "name": "Bench", "domain": DOMAIN})),
        Some(&session),
        "bench",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{site}");
    let site_id = site["id"].as_str().unwrap().to_owned();
    let public_id = site["public_id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE instance_settings SET analytics_daily_quota = 1000000000000 WHERE id = 1")
        .execute(state.db.pool())
        .await
        .unwrap();

    let now = owlee_insights::now_secs();
    let load = bulk_load(&state, &site_id, events, now, backend == "postgres").await;
    println!("bulk load: {events} rows in {load:.1?}");

    ingest(&app, &public_id, ingest_total, 1).await;
    ingest(&app, &public_id, ingest_total, concurrency).await;

    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM insight_events WHERE site_id = $1")
        .bind(&site_id)
        .fetch_one(state.db.pool())
        .await
        .unwrap();
    println!("rows stored: {stored}");
    assert_eq!(
        stored as usize,
        events + 2 * ingest_total,
        "every row was written"
    );

    let base = format!("/api/sites/{site_id}");
    for days in [7, 30, 90] {
        let now = owlee_insights::now_secs();
        let q = format!("from={}&to={}", now - days * DAY, now + 1);
        let in_range: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM insight_events WHERE site_id = $1 AND timestamp >= $2",
        )
        .bind(&site_id)
        .bind(now - days * DAY)
        .fetch_one(state.db.pool())
        .await
        .unwrap();
        println!("\n== {days} days ({in_range} rows in range) ==");

        // What the analytics page sends at once when it opens.
        let page = [
            format!("{base}/overview?{q}"),
            format!("{base}/breakdown/path?{q}"),
            format!("{base}/breakdown/referrer?{q}"),
            format!("{base}/breakdown/country?{q}"),
            format!("{base}/breakdown/browser?{q}"),
            format!("{base}/vitals?{q}"),
            format!("{base}/realtime"),
        ];
        let mut walls = Vec::new();
        for _ in 0..runs {
            let t = Instant::now();
            let all =
                futures_util::future::join_all(page.iter().map(|uri| timed(&app, &session, uri)))
                    .await;
            walls.push(t.elapsed());
            let path_rows = all[1].1["rows"].as_array().map_or(0, Vec::len);
            assert!(path_rows > 0, "the pages breakdown has rows");
        }
        let (m, lo, hi) = median(walls);
        println!(
            "page load (7 requests at once): median {m:.2?}, range {lo:.2?} to {hi:.2?}, {runs} runs"
        );

        let single = [
            format!("{base}/overview?{q}"),
            format!("{base}/breakdown/path?{q}"),
            format!("{base}/breakdown/referrer?{q}"),
            format!("{base}/breakdown/country?{q}"),
            format!("{base}/breakdown/browser?{q}"),
            format!("{base}/breakdown/entry?{q}"),
            format!("{base}/vitals?{q}"),
            format!("{base}/events?{q}"),
            format!("{base}/visitors?{q}"),
            format!("{base}/realtime"),
            "/api/sites".to_owned(),
        ];
        for uri in &single {
            let mut times = Vec::new();
            for _ in 0..3 {
                times.push(timed(&app, &session, uri).await.0);
            }
            let (m, lo, hi) = median(times);
            let name = uri.split('?').next().unwrap().replace(&base, "");
            println!("  {name:<22} median {m:>9.2?} (range {lo:.2?} to {hi:.2?})");
        }
    }
}
