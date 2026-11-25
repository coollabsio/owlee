//! Funnel benchmark for the open question in
//! `docs/specs/2026-11-23-product-analytics-design.md` §16: is 2,000,000 rows a good scan limit?
//!
//! It prototypes the query engine of §7 (SQL filter, then a pure Rust pass) on the real schema.
//! Not part of the normal test run:
//!
//! ```sh
//! cargo test --release -p owlee-server --test funnel_bench -- --ignored --nocapture
//! BENCH_POSTGRES_URL=postgres://... cargo test --release -p owlee-server --test funnel_bench -- --ignored --nocapture
//! ```
//!
//! `BENCH_ROWS` sets the matching rows (default 2,000,000). `BENCH_RUNS` sets runs per mode (default 5).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use owlee_server::{AppState, boot, editor_session, testing_config};
use sqlx::Row;

const DAY: i64 = 86_400;
const RANGE_DAYS: i64 = 30;
const STEPS: usize = 4;

/// Deterministic xorshift, so every backend gets the same data.
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
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

struct EventRow {
    id: String,
    timestamp: i64,
    event_type: &'static str,
    session_id: String,
    visit_id: String,
    pathname: Option<String>,
    referrer: &'static str,
    country: &'static str,
    browser: &'static str,
}

struct Seeded {
    site_id: String,
    from: i64,
    to: i64,
    matching_rows: usize,
    total_rows: usize,
    expected_visit: [u64; STEPS],
}

/// Events, signup data rows `(event_id, timestamp, plan)`, expected visit result, matching rows.
type Generated = (
    Vec<EventRow>,
    Vec<(String, i64, &'static str)>,
    [u64; STEPS],
    usize,
);

/// Funnel: pageview `/` (exact) → pageview `/pricing` (exact) → pageview `/docs/**` (glob)
/// → event `signup` where `plan = pro`. The glob step means no pageview can be dropped in SQL,
/// so every pageview and every signup is a matching row: the worst case of §7.
fn generate(target_matching: usize, now: i64) -> Generated {
    const NOISE: [&str; 4] = [
        "/blog/launch",
        "/blog/pricing-change",
        "/about",
        "/changelog",
    ];
    const DOCS: [&str; 3] = ["/docs/install", "/docs/api/tokens", "/docs/errors/replay"];
    const REFERRERS: [&str; 4] = ["", "google.com", "github.com", "news.ycombinator.com"];
    const COUNTRIES: [&str; 4] = ["US", "DE", "HU", "IN"];
    const BROWSERS: [&str; 3] = ["Chrome", "Firefox", "Safari"];

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let from = now - RANGE_DAYS * DAY;
    let mut events = Vec::with_capacity(target_matching + target_matching / 5);
    let mut data = Vec::new();
    let mut expected = [0_u64; STEPS];
    let mut matching = 0_usize;
    let mut visitor = 0_u64;
    let mut visit = 0_u64;

    'outer: loop {
        visitor += 1;
        let session_id = format!("{visitor:016x}");
        let visits = 1 + rng.below(3);
        let mut ts = from + rng.below((RANGE_DAYS - 2) as u64 * DAY as u64) as i64;
        for _ in 0..visits {
            visit += 1;
            let visit_id = format!("{visit:016x}{visitor:016x}");
            let referrer = REFERRERS[rng.below(4) as usize];
            let country = COUNTRIES[rng.below(4) as usize];
            let browser = BROWSERS[rng.below(3) as usize];
            let mut rows: Vec<(&'static str, Option<String>, Option<bool>)> = Vec::new();
            // Depth reached: 70% start the funnel, then 50% go on at each step.
            let mut depth = 0;
            if rng.chance(70) {
                depth = 1;
                while depth < STEPS && rng.chance(50) {
                    depth += 1;
                }
            }
            if depth == 0 {
                rows.push((
                    "pageview",
                    Some(NOISE[rng.below(4) as usize].to_owned()),
                    None,
                ));
            }
            for step in 0..depth {
                for _ in 0..rng.below(3) {
                    rows.push((
                        "pageview",
                        Some(NOISE[rng.below(4) as usize].to_owned()),
                        None,
                    ));
                }
                match step {
                    0 => rows.push(("pageview", Some("/".to_owned()), None)),
                    1 => rows.push(("pageview", Some("/pricing".to_owned()), None)),
                    2 => rows.push((
                        "pageview",
                        Some(DOCS[rng.below(3) as usize].to_owned()),
                        None,
                    )),
                    _ => rows.push(("signup", None, Some(rng.chance(60)))),
                }
            }
            if matching + rows.len() > target_matching {
                break 'outer;
            }
            // Expected visit-mode result: a free signup stops the funnel at step 3.
            for (step, slot) in expected.iter_mut().enumerate().take(depth) {
                if step == 3 && rows.iter().any(|r| r.2 == Some(false)) {
                    break;
                }
                *slot += 1;
            }
            for (event_type, pathname, pro) in rows {
                // 0..=90 s apart; 0 tests the same-second rule.
                ts += rng.below(91) as i64;
                let id = owlee_platform::Id::new().to_string();
                if let Some(pro) = pro {
                    data.push((id.clone(), ts, if pro { "pro" } else { "free" }));
                }
                events.push(EventRow {
                    id,
                    timestamp: ts,
                    event_type,
                    session_id: session_id.clone(),
                    visit_id: visit_id.clone(),
                    pathname,
                    referrer,
                    country,
                    browser,
                });
                matching += 1;
                // Web vitals rows: stored, but filtered out by SQL.
                if event_type == "pageview" && rng.chance(20) {
                    events.push(EventRow {
                        id: owlee_platform::Id::new().to_string(),
                        timestamp: ts,
                        event_type: "performance",
                        session_id: session_id.clone(),
                        visit_id: visit_id.clone(),
                        pathname: None,
                        referrer,
                        country,
                        browser,
                    });
                }
            }
            ts += 3_600 + rng.below(DAY as u64) as i64;
        }
    }
    (events, data, expected, matching)
}

async fn seed(state: &AppState, target_matching: usize) -> Seeded {
    let session = editor_session(state).await.unwrap();
    let now = owlee_insights::now_secs();
    let site_id = owlee_platform::Id::new().to_string();
    sqlx::query(
        "INSERT INTO insight_sites (id, project_id, public_id, name, domain, public_slug, public_sections, created_at) \
         VALUES ($1, $2, $3, 'bench', $4, $5, '[]', $6)",
    )
    .bind(&site_id)
    .bind(session.project_id.to_string())
    .bind(format!("pub_{site_id}"))
    .bind(format!("{site_id}.bench.test"))
    .bind(format!("bench-{site_id}"))
    .bind(now)
    .execute(state.db.pool())
    .await
    .unwrap();

    let (events, data, expected, matching) = generate(target_matching, now);
    let started = Instant::now();
    const COLUMNS: usize = 10;
    const BATCH: usize = 500;
    for chunk in events.chunks(BATCH) {
        let mut sql = String::from(
            "INSERT INTO insight_events (id, site_id, timestamp, event_type, session_id, visit_id, pathname, referrer_domain, country, browser) VALUES ",
        );
        for row in 0..chunk.len() {
            if row > 0 {
                sql.push(',');
            }
            sql.push('(');
            for col in 0..COLUMNS {
                if col > 0 {
                    sql.push(',');
                }
                sql.push_str(&format!("${}", row * COLUMNS + col + 1));
            }
            sql.push(')');
        }
        let mut query = sqlx::query(&sql);
        for e in chunk {
            query = query
                .bind(&e.id)
                .bind(&site_id)
                .bind(e.timestamp)
                .bind(e.event_type)
                .bind(&e.session_id)
                .bind(&e.visit_id)
                .bind(e.pathname.as_deref())
                .bind(e.referrer)
                .bind(e.country)
                .bind(e.browser);
        }
        query.execute(state.db.pool()).await.unwrap();
    }
    for chunk in data.chunks(BATCH) {
        let mut sql = String::from(
            "INSERT INTO insight_event_data (id, site_id, event_id, event_name, timestamp, key, string_value, data_type) VALUES ",
        );
        for row in 0..chunk.len() {
            if row > 0 {
                sql.push(',');
            }
            let b = row * 5;
            sql.push_str(&format!(
                "(${}, ${}, ${}, 'signup', ${}, 'plan', ${}, 1)",
                b + 1,
                b + 2,
                b + 3,
                b + 4,
                b + 5
            ));
        }
        let mut query = sqlx::query(&sql);
        for (event_id, ts, plan) in chunk {
            query = query
                .bind(owlee_platform::Id::new().to_string())
                .bind(&site_id)
                .bind(event_id)
                .bind(*ts)
                .bind(*plan);
        }
        query.execute(state.db.pool()).await.unwrap();
    }
    // The index §7 names. Not in a migration yet.
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS insight_events_site_type ON insight_events (site_id, event_type, timestamp)",
    )
    .execute(state.db.pool())
    .await
    .unwrap();
    sqlx::query("ANALYZE")
        .execute(state.db.pool())
        .await
        .unwrap();
    println!(
        "seeded {} event rows ({} matching) and {} data rows in {:.1?}",
        events.len(),
        matching,
        data.len(),
        started.elapsed()
    );
    Seeded {
        site_id,
        from: now - RANGE_DAYS * DAY,
        to: now + DAY,
        matching_rows: matching,
        total_rows: events.len(),
        expected_visit: expected,
    }
}

#[derive(Clone, Copy, Debug)]
enum Actor {
    Visit,
    Visitor { window: i64 },
}

#[derive(Debug, PartialEq)]
struct FunnelResult {
    reached: [u64; STEPS],
    median_secs: [Option<i64>; STEPS],
}

/// Pure pass of §7: events of one actor, sorted by (timestamp, step), so a step at the same
/// second as the step before counts. Keeps, for each level, the chain with the latest start,
/// because a later start leaves the most of the window.
fn funnel(groups: HashMap<String, Vec<(i64, u8)>>, window: i64) -> FunnelResult {
    let mut reached = [0_u64; STEPS];
    let mut durations: [Vec<i64>; STEPS] = Default::default();
    for (_, mut events) in groups {
        events.sort_unstable();
        let mut level: [Option<(i64, i64)>; STEPS] = [None; STEPS];
        let mut first_reach: [Option<i64>; STEPS] = [None; STEPS];
        for (ts, step) in events {
            let step = step as usize;
            let candidate = if step == 0 {
                Some((ts, ts))
            } else {
                level[step - 1]
                    .filter(|(start, _)| ts - start <= window)
                    .map(|(start, _)| (start, ts))
            };
            let Some((start, at)) = candidate else {
                continue;
            };
            if level[step].is_none_or(|(old, _)| start > old) {
                if first_reach[step].is_none() {
                    first_reach[step] = Some(at);
                    if step > 0 {
                        durations[step].push(at - level[step - 1].unwrap().1);
                    }
                }
                level[step] = Some((start, at));
            }
        }
        for (step, slot) in reached.iter_mut().enumerate() {
            if first_reach[step].is_some() {
                *slot += 1;
            }
        }
    }
    let median_secs = durations.map(|mut d| {
        if d.is_empty() {
            return None;
        }
        let mid = d.len() / 2;
        Some(*d.select_nth_unstable(mid).1)
    });
    FunnelResult {
        reached,
        median_secs,
    }
}

#[derive(Debug)]
struct Timing {
    rows: usize,
    fetch: Duration,
    compute: Duration,
    result: FunnelResult,
}

async fn run(
    state: &AppState,
    seeded: &Seeded,
    actor: Actor,
    max_rows: usize,
) -> Result<Timing, String> {
    let started = Instant::now();
    let pro: HashSet<String> = sqlx::query(
        "SELECT event_id FROM insight_event_data WHERE site_id = $1 AND event_name = 'signup' \
         AND timestamp >= $2 AND timestamp < $3 AND key = 'plan' AND string_value = 'pro'",
    )
    .bind(&seeded.site_id)
    .bind(seeded.from)
    .bind(seeded.to)
    .fetch(state.db.pool())
    .map_ok(|row| row.get::<String, _>(0))
    .try_collect()
    .await
    .map_err(|e| e.to_string())?;

    let mut rows = sqlx::query(
        "SELECT id, visit_id, session_id, timestamp, event_type, pathname FROM insight_events \
         WHERE site_id = $1 AND timestamp >= $2 AND timestamp < $3 AND event_type IN ('pageview', 'signup')",
    )
    .bind(&seeded.site_id)
    .bind(seeded.from)
    .bind(seeded.to)
    .fetch(state.db.pool());
    let mut groups: HashMap<String, Vec<(i64, u8)>> = HashMap::new();
    let mut count = 0_usize;
    while let Some(row) = rows.try_next().await.map_err(|e| e.to_string())? {
        count += 1;
        if count > max_rows {
            return Err("422: too many events".to_owned());
        }
        let event_type: &str = row.get(4);
        let step = if event_type == "signup" {
            let id: &str = row.get(0);
            pro.contains(id).then_some(3)
        } else {
            match row.get::<Option<&str>, _>(5) {
                Some("/") => Some(0),
                Some("/pricing") => Some(1),
                Some(path) if path.starts_with("/docs/") => Some(2),
                _ => None,
            }
        };
        let Some(step) = step else { continue };
        let key: String = match actor {
            Actor::Visit => row.get(1),
            Actor::Visitor { .. } => row.get(2),
        };
        groups.entry(key).or_default().push((row.get(3), step));
    }
    drop(rows);
    let fetch = started.elapsed();
    let started = Instant::now();
    let window = match actor {
        Actor::Visit => i64::MAX,
        Actor::Visitor { window } => window,
    };
    let result = funnel(groups, window);
    Ok(Timing {
        rows: count,
        fetch,
        compute: started.elapsed(),
        result,
    })
}

fn reset_peak_rss() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn peak_rss_mib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

fn summary(label: &str, totals: &mut [Duration]) {
    totals.sort();
    println!(
        "{label}: median {:.2?}, range {:.2?} to {:.2?}, {} runs",
        totals[totals.len() / 2],
        totals[0],
        totals[totals.len() - 1],
        totals.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark: run with --ignored --release"]
async fn funnel_over_two_million_rows() {
    let target: usize = std::env::var("BENCH_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000_000);
    let runs: usize = std::env::var("BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("funnel-bench");
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
    let state = boot(config).await.unwrap();
    println!("backend: {backend}");
    let seeded = seed(&state, target).await;
    println!("expected (visit): {:?}", seeded.expected_visit);

    // Unreported: DB scan alone, to split database time from Rust time.
    let started = Instant::now();
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM insight_events WHERE site_id = $1 AND timestamp >= $2 AND timestamp < $3 \
         AND event_type IN ('pageview', 'signup')",
    )
    .bind(&seeded.site_id)
    .bind(seeded.from)
    .bind(seeded.to)
    .fetch_one(state.db.pool())
    .await
    .unwrap();
    println!(
        "COUNT(*) of matching rows: {n} in {:.2?}",
        started.elapsed()
    );
    assert_eq!(n as usize, seeded.matching_rows);

    let visitor = Actor::Visitor { window: DAY };
    let mut visit_totals = Vec::new();
    let mut visitor_totals = Vec::new();
    let mut errors = 0;
    for i in 0..runs {
        // Alternate the modes so warmup and cache favor neither.
        for actor in [Actor::Visit, visitor] {
            reset_peak_rss();
            match run(&state, &seeded, actor, 2_000_000).await {
                Ok(t) => {
                    println!(
                        "run {i} {actor:?}: rows {} fetch {:.2?} compute {:.2?} peak RSS {} MiB reached {:?} medians {:?}",
                        t.rows,
                        t.fetch,
                        t.compute,
                        peak_rss_mib(),
                        t.result.reached,
                        t.result.median_secs
                    );
                    assert_eq!(t.rows, seeded.matching_rows, "every matching row was read");
                    if let Actor::Visit = actor {
                        assert_eq!(
                            t.result.reached, seeded.expected_visit,
                            "visit funnel is correct"
                        );
                        visit_totals.push(t.fetch + t.compute);
                    } else {
                        assert!(t.result.reached[0] > 0);
                        visitor_totals.push(t.fetch + t.compute);
                    }
                }
                Err(e) => {
                    errors += 1;
                    println!("run {i} {actor:?}: error {e}");
                }
            }
        }
    }
    summary("visit funnel", &mut visit_totals);
    summary("visitor funnel (1 day window)", &mut visitor_totals);

    // §7 allows 2 reports at once: both share the pool and the CPU.
    let started = Instant::now();
    let (a, b) = tokio::join!(
        run(&state, &seeded, Actor::Visit, 2_000_000),
        run(&state, &seeded, visitor, 2_000_000)
    );
    println!(
        "2 concurrent reports: wall {:.2?}, ok {} {}",
        started.elapsed(),
        a.is_ok(),
        b.is_ok()
    );

    // The limit itself: one row over answers 422.
    let over = run(&state, &seeded, Actor::Visit, seeded.matching_rows - 1).await;
    assert!(over.is_err(), "a range over the limit fails");

    println!(
        "errors: {errors}, total rows in table: {}",
        seeded.total_rows
    );
    assert_eq!(errors, 0);
}
