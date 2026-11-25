//! Session replay intake, storage, caps, routes, and privacy over HTTP.

use std::io::Write;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use http_body_util::BodyExt;
use owlee_platform::Config;
use owlee_server::{
    AppState, TestSession, boot, editor_session, member_session, prune_replays, router,
    testing_config,
};
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;

const RECORDING: &[u8] = br#"[{"type":4,"data":{"href":"https://shop.example/"},"timestamp":1}]"#;

struct Hit {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}

impl Hit {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }

    fn rate_limits(&self) -> Option<&str> {
        self.headers
            .get("x-sentry-rate-limits")
            .and_then(|value| value.to_str().ok())
    }
}

struct Harness {
    app: Router,
    state: AppState,
    editor: TestSession,
}

struct ErrorApp {
    id: String,
    key: String,
}

fn sqlite_config() -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    testing_config(path)
}

/// A unique replay id per call, so reused Postgres databases never collide.
fn replay_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn zlib(body: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}

struct Segment<'a> {
    replay_id: &'a str,
    segment_id: u64,
    replay_type: &'a str,
    timestamp: f64,
    recording: Vec<u8>,
    url: &'a str,
}

impl<'a> Segment<'a> {
    fn new(replay_id: &'a str, segment_id: u64) -> Self {
        Self {
            replay_id,
            segment_id,
            replay_type: "session",
            timestamp: chrono::Utc::now().timestamp() as f64,
            recording: zlib(RECORDING),
            url: "https://shop.example/cart?token=secret",
        }
    }

    fn replay_event(&self) -> Value {
        json!({
            "type": "replay_event",
            "replay_id": self.replay_id,
            "event_id": self.replay_id,
            "replay_type": self.replay_type,
            "segment_id": self.segment_id,
            "timestamp": self.timestamp,
            "replay_start_timestamp": self.timestamp - 1.0,
            "urls": [self.url],
            "release": "shop@2.0",
            "environment": "production",
            "user": { "id": "42", "email": "ada@example.com", "ip_address": "203.0.113.9" },
            "request": { "url": self.url, "headers": { "User-Agent": "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0" } },
        })
    }
}

/// An envelope with the segment's two items, and an error event when given.
fn envelope(segment: &Segment, event: Option<Value>) -> Vec<u8> {
    let mut body = format!("{{\"event_id\":\"{}\"}}\n", segment.replay_id).into_bytes();
    if let Some(event) = event {
        let event = event.to_string();
        body.extend_from_slice(
            format!(
                "{{\"type\":\"event\",\"length\":{}}}\n{event}\n",
                event.len()
            )
            .as_bytes(),
        );
    }
    let replay_event = segment.replay_event().to_string();
    body.extend_from_slice(
        format!(
            "{{\"type\":\"replay_event\",\"length\":{}}}\n{replay_event}\n",
            replay_event.len()
        )
        .as_bytes(),
    );
    let mut recording = format!("{{\"segment_id\":{}}}\n", segment.segment_id).into_bytes();
    recording.extend_from_slice(&segment.recording);
    body.extend_from_slice(
        format!(
            "{{\"type\":\"replay_recording\",\"length\":{}}}\n",
            recording.len()
        )
        .as_bytes(),
    );
    body.extend_from_slice(&recording);
    body.push(b'\n');
    body
}

fn error_event(replay_id: &str) -> Value {
    json!({
        "event_id": uuid::Uuid::new_v4().simple().to_string(),
        "level": "error",
        "platform": "javascript",
        "tags": { "replayId": replay_id },
        "exception": { "values": [{ "type": "TypeError", "value": format!("cart is undefined {replay_id}") }] },
    })
}

impl Harness {
    async fn new(config: Config) -> Self {
        let state = boot(config).await.expect("boot");
        let editor = editor_session(&state).await.unwrap();
        Self {
            app: router(state.clone()),
            state,
            editor,
        }
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        body: Vec<u8>,
        headers: &[(&str, String)],
    ) -> Hit {
        let peer = std::net::SocketAddr::from(([198, 51, 100, 7], 40000));
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .extension(axum::extract::ConnectInfo(peer));
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Hit {
            status,
            headers,
            bytes: bytes.to_vec(),
        }
    }

    fn cookie(session: &TestSession) -> Vec<(&'static str, String)> {
        vec![
            ("cookie", session.cookie.clone()),
            ("x-owlee-workspace", session.workspace_id.to_string()),
            ("content-type", "application/json".to_owned()),
        ]
    }

    async fn as_editor(&self, method: &str, uri: &str, body: Option<Value>) -> Hit {
        let body = body
            .map(|body| body.to_string().into_bytes())
            .unwrap_or_default();
        self.send(method, uri, body, &Self::cookie(&self.editor))
            .await
    }

    async fn error_app(&self, replays: bool) -> ErrorApp {
        let created = self
            .as_editor(
                "POST",
                "/api/error-apps",
                Some(json!({ "name": "Shop", "platform": "javascript", "project_id": self.editor.project_id })),
            )
            .await;
        assert_eq!(created.status, StatusCode::OK);
        let body = created.json();
        let app = ErrorApp {
            id: body["id"].as_str().unwrap().to_owned(),
            key: body["publicKey"].as_str().unwrap().to_owned(),
        };
        assert_eq!(
            body["replays"],
            json!({ "enabled": false, "dailyBytes": 536_870_912, "hourlyLimit": 1000, "todayBytes": 0, "hourReplays": 0, "totalBytes": 0 })
        );
        if replays {
            self.settings(
                &app,
                json!({ "enabled": true, "dailyBytes": 536_870_912, "hourlyLimit": 1000 }),
            )
            .await;
        }
        app
    }

    async fn settings(&self, app: &ErrorApp, body: Value) -> Hit {
        let hit = self
            .as_editor(
                "PUT",
                &format!("/api/error-apps/{}/replays-settings", app.id),
                Some(body),
            )
            .await;
        if hit.status == StatusCode::OK {
            assert_eq!(hit.json()["id"], app.id);
        }
        hit
    }

    async fn ingest(&self, app: &ErrorApp, body: Vec<u8>) -> Hit {
        self.send(
            "POST",
            &format!("/api/{}/envelope/?sentry_key={}", app.id, app.key),
            body,
            &[("content-type", "application/x-sentry-envelope".to_owned())],
        )
        .await
    }

    async fn replay(&self, app: &ErrorApp, replay_id: &str) -> Hit {
        self.as_editor(
            "GET",
            &format!("/api/error-apps/{}/replays/{replay_id}", app.id),
            None,
        )
        .await
    }

    async fn count(&self, sql: &str, app: &ErrorApp) -> i64 {
        sqlx::query(sql)
            .bind(&app.id)
            .fetch_one(self.state.db.pool())
            .await
            .unwrap()
            .try_get::<i64, _>(0)
            .unwrap()
    }

    async fn usage(&self, app: &ErrorApp) -> (i64, i64) {
        let bytes = self
            .count(
                "SELECT CAST(COALESCE(SUM(bytes), 0) AS BIGINT) FROM replay_daily_usage WHERE app_id = $1",
                app,
            )
            .await;
        let replays = self
            .count(
                "SELECT CAST(COALESCE(SUM(replays), 0) AS BIGINT) FROM replay_hourly_usage WHERE app_id = $1",
                app,
            )
            .await;
        (bytes, replays)
    }

    async fn intake_drop(&self, app: &ErrorApp) -> Value {
        let view = self
            .as_editor("GET", &format!("/api/error-apps/{}", app.id), None)
            .await;
        view.json()["intake"]["last_drop"]["reason"].clone()
    }
}

async fn exercise(config: Config) {
    stores_a_segment_once(config.clone()).await;
    out_of_order_and_buffer_to_session(config.clone()).await;
    clamps_future_times_and_retention_uses_the_server_clock(config.clone()).await;
    hourly_cap_drops_new_replays_only(config.clone()).await;
    daily_bytes_cap(config.clone()).await;
    disabled_and_unsupported_and_malformed(config.clone()).await;
    replay_only_envelopes_use_their_own_buckets(config.clone()).await;
    ip_refusal_before_the_body_names_both_categories(config.clone()).await;
    body_limits(config.clone()).await;
    routes_are_scoped_to_the_app_and_workspace(config.clone()).await;
    api_token_scopes(config.clone()).await;
    errors_link_to_replays(config.clone()).await;
    list_filters_and_pages(config.clone()).await;
    settings_are_validated(config.clone()).await;
    privacy_requests_find_replays(config).await;
}

async fn stores_a_segment_once(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    let segment = Segment::new(&id, 0);
    let first = h.ingest(&app, envelope(&segment, None)).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.rate_limits(), None);
    let before = h.replay(&app, &id).await.json();
    let usage = h.usage(&app).await;
    assert_eq!(usage, (segment.recording.len() as i64, 1));

    let retry = h.ingest(&app, envelope(&segment, None)).await;
    assert_eq!(retry.status, StatusCode::OK);
    let after = h.replay(&app, &id).await.json();
    assert_eq!(after["segmentCount"], 1);
    assert_eq!(after["bytes"], segment.recording.len());
    assert_eq!(after["segments"], json!([0]));
    assert_eq!(after["lastReceivedMs"], before["lastReceivedMs"]);
    assert_eq!(h.usage(&app).await, usage, "a retry changes no counter");
    assert_eq!(
        after["urls"],
        json!(["https://shop.example/cart?token=[Filtered]"])
    );
    assert_eq!(
        after["firstUrl"],
        "https://shop.example/cart?token=[Filtered]"
    );
    assert_eq!(after["browser"], "Firefox");
    assert_eq!(after["userLabel"], "ada@example.com");
    assert_eq!(after["release"], "shop@2.0");
    assert_eq!(after["live"], true);

    let segment_hit = h
        .as_editor(
            "GET",
            &format!("/api/error-apps/{}/replays/{id}/segments/0", app.id),
            None,
        )
        .await;
    assert_eq!(segment_hit.status, StatusCode::OK);
    assert_eq!(segment_hit.headers["content-encoding"], "deflate");
    assert_eq!(segment_hit.headers["content-type"], "application/json");
    assert_eq!(segment_hit.headers["cache-control"], "no-store");
    assert_eq!(segment_hit.bytes, segment.recording);

    let mut plain = Segment::new(&id, 1);
    plain.recording = RECORDING.to_vec();
    assert_eq!(
        h.ingest(&app, envelope(&plain, None)).await.status,
        StatusCode::OK
    );
    let plain_hit = h
        .as_editor(
            "GET",
            &format!("/api/error-apps/{}/replays/{id}/segments/1", app.id),
            None,
        )
        .await;
    assert!(plain_hit.headers.get("content-encoding").is_none());
    assert_eq!(plain_hit.bytes, RECORDING);

    let view = h
        .as_editor("GET", &format!("/api/error-apps/{}", app.id), None)
        .await
        .json();
    let total = (segment.recording.len() + RECORDING.len()) as i64;
    assert_eq!(view["replays"]["totalBytes"], total);
    assert_eq!(view["replays"]["todayBytes"], total);
    assert_eq!(view["replays"]["hourReplays"], 1);
    assert_eq!(
        h.count(
            "SELECT CAST(COUNT(*) AS BIGINT) FROM replay_users WHERE app_id = $1",
            &app
        )
        .await,
        2,
        "id and email are hashed, the IP address is not"
    );
}

async fn out_of_order_and_buffer_to_session(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    let mut third = Segment::new(&id, 3);
    third.replay_type = "buffer";
    h.ingest(&app, envelope(&third, None)).await;
    let early = h.replay(&app, &id).await.json();
    assert_eq!(early["hasStart"], false);
    assert_eq!(early["startedMs"], Value::Null);
    assert_eq!(early["durationMs"], Value::Null);
    assert_eq!(early["replayType"], "buffer");

    let mut first = Segment::new(&id, 0);
    first.replay_type = "buffer";
    first.timestamp = third.timestamp - 10.0;
    h.ingest(&app, envelope(&first, None)).await;
    let started = h.replay(&app, &id).await.json();
    assert_eq!(started["hasStart"], true);
    let started_ms = (first.timestamp - 1.0) * 1000.0;
    assert_eq!(started["startedMs"].as_f64().unwrap(), started_ms);
    assert_eq!(started["segments"], json!([0, 3]));
    assert_eq!(started["replayType"], "buffer");
    assert_eq!(
        started["lastMs"].as_f64().unwrap(),
        third.timestamp * 1000.0
    );

    h.ingest(&app, envelope(&Segment::new(&id, 4), None)).await;
    assert_eq!(h.replay(&app, &id).await.json()["replayType"], "session");
    let mut later_buffer = Segment::new(&id, 5);
    later_buffer.replay_type = "buffer";
    h.ingest(&app, envelope(&later_buffer, None)).await;
    let done = h.replay(&app, &id).await.json();
    assert_eq!(done["replayType"], "session", "session stays session");
    assert_eq!(done["segmentCount"], 4);
    assert_eq!(
        h.usage(&app).await.1,
        1,
        "one new replay, whatever the order"
    );
}

async fn clamps_future_times_and_retention_uses_the_server_clock(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    let mut future = Segment::new(&id, 0);
    future.timestamp = 4_102_444_800.0;
    h.ingest(&app, envelope(&future, None)).await;
    let replay = h.replay(&app, &id).await.json();
    let received = replay["lastReceivedMs"].as_i64().unwrap();
    assert_eq!(replay["lastMs"].as_i64().unwrap(), received + 5 * 60 * 1000);
    assert_eq!(
        replay["startedMs"].as_i64().unwrap(),
        received + 5 * 60 * 1000
    );

    let fourteen_days = 14 * 86_400_000;
    prune_replays(&h.state.db, received - fourteen_days)
        .await
        .unwrap();
    assert_eq!(h.replay(&app, &id).await.status, StatusCode::OK);
    // 14 days later, by the server clock, the replay is due.
    prune_replays(&h.state.db, received + 1).await.unwrap();
    assert_eq!(h.replay(&app, &id).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        h.count(
            "SELECT CAST(COUNT(*) AS BIGINT) FROM replay_segments WHERE app_id = $1",
            &app
        )
        .await,
        0
    );
    assert_eq!(
        h.count(
            "SELECT CAST(COUNT(*) AS BIGINT) FROM replay_urls WHERE app_id = $1",
            &app
        )
        .await,
        0
    );
}

async fn hourly_cap_drops_new_replays_only(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let known = replay_id();
    h.ingest(&app, envelope(&Segment::new(&known, 0), None))
        .await;
    let hour = {
        let now = chrono::Utc::now().timestamp();
        now - now.rem_euclid(3600)
    };
    sqlx::query("UPDATE replay_hourly_usage SET replays = 1000 WHERE app_id = $1 AND hour = $2")
        .bind(&app.id)
        .bind(hour)
        .execute(h.state.db.pool())
        .await
        .unwrap();

    let fresh = replay_id();
    let refused = h
        .ingest(
            &app,
            envelope(&Segment::new(&fresh, 0), Some(error_event(&fresh))),
        )
        .await;
    assert_eq!(refused.status, StatusCode::OK);
    assert!(refused.json()["id"].is_string(), "the event is stored");
    let limits = refused.rate_limits().unwrap();
    assert!(
        limits.contains(":replay:project:replay_hourly_limit"),
        "{limits}"
    );
    assert!(!limits.contains(":error:"), "{limits}");
    assert_eq!(h.replay(&app, &fresh).await.status, StatusCode::NOT_FOUND);
    assert_eq!(h.intake_drop(&app).await, "replay_hourly_limit");

    let next = h
        .ingest(&app, envelope(&Segment::new(&known, 1), None))
        .await;
    assert_eq!(next.rate_limits(), None);
    assert_eq!(h.replay(&app, &known).await.json()["segmentCount"], 2);
    assert_eq!(h.usage(&app).await.1, 1000);
}

async fn daily_bytes_cap(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(false).await;
    let saved = h
        .settings(
            &app,
            json!({ "enabled": true, "dailyBytes": 1_048_576, "hourlyLimit": 10 }),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_eq!(
        saved.json()["replays"]["dailyBytes"],
        1_048_576,
        "the settings answer is the app view"
    );
    let first = replay_id();
    h.ingest(&app, envelope(&Segment::new(&first, 0), None))
        .await;
    sqlx::query("UPDATE replay_daily_usage SET bytes = $1 WHERE app_id = $2")
        .bind(1_048_576_i64 - 5)
        .bind(&app.id)
        .execute(h.state.db.pool())
        .await
        .unwrap();
    let refused = h
        .ingest(
            &app,
            envelope(&Segment::new(&first, 1), Some(error_event(&first))),
        )
        .await;
    assert_eq!(refused.status, StatusCode::OK);
    assert!(refused.json()["id"].is_string(), "the event is stored");
    let limits = refused.rate_limits().unwrap();
    assert!(
        limits.contains(":replay:organization:quota_exceeded"),
        "{limits}"
    );
    assert_eq!(h.intake_drop(&app).await, "replay_quota");
    let replay = h.replay(&app, &first).await.json();
    assert_eq!(replay["segments"], json!([0]));
    assert_eq!(replay["errorCount"], 1);
    assert_eq!(h.usage(&app).await.0, 1_048_576 - 5);
}

async fn disabled_and_unsupported_and_malformed(config: Config) {
    let h = Harness::new(config).await;
    let off = h.error_app(false).await;
    let id = replay_id();
    let hit = h.ingest(&off, envelope(&Segment::new(&id, 0), None)).await;
    assert_eq!(hit.status, StatusCode::OK);
    assert_eq!(
        hit.rate_limits(),
        Some("86400:replay:project:replays_disabled")
    );
    assert_eq!(h.intake_drop(&off).await, "replays_disabled");
    assert_eq!(h.replay(&off, &id).await.status, StatusCode::NOT_FOUND);

    let on = h.error_app(true).await;
    let video = format!(
        "{{}}\n{{\"type\":\"replay_event\"}}\n{}\n{{\"type\":\"replay_video\",\"length\":3}}\nabc\n",
        Segment::new(&id, 0).replay_event()
    );
    let hit = h.ingest(&on, video.into_bytes()).await;
    assert_eq!(hit.status, StatusCode::OK);
    assert_eq!(hit.rate_limits(), None);
    assert_eq!(h.intake_drop(&on).await, "unsupported");

    let mut gzip = Segment::new(&id, 0);
    gzip.recording = vec![0x1f, 0x8b, 0x08, 0x00];
    let hit = h.ingest(&on, envelope(&gzip, None)).await;
    assert_eq!(hit.status, StatusCode::OK);
    assert_eq!(h.intake_drop(&on).await, "malformed");
    let mut big_segment = Segment::new(&id, 1000);
    big_segment.recording = RECORDING.to_vec();
    h.ingest(&on, envelope(&big_segment, None)).await;
    assert_eq!(h.intake_drop(&on).await, "malformed");
    assert_eq!(h.replay(&on, &id).await.status, StatusCode::NOT_FOUND);

    let mut local = Segment::new(&id, 0);
    local.url = "http://localhost:3000/";
    let saved = h
        .as_editor(
            "PUT",
            &format!("/api/error-apps/{}/protection", on.id),
            Some(json!({ "allowedDomains": [], "inboundFilters": ["localhost"], "spikeProtection": true })),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK);
    h.ingest(&on, envelope(&local, None)).await;
    assert_eq!(h.intake_drop(&on).await, "filtered");
    assert_eq!(h.replay(&on, &id).await.status, StatusCode::NOT_FOUND);
}

async fn replay_only_envelopes_use_their_own_buckets(mut config: Config) {
    config.ingest.error_rate_project_per_min = 1;
    config.ingest.error_rate_project_burst = 2;
    config.ingest.error_rate_ip_per_min = 1;
    config.ingest.error_rate_ip_burst = 2;
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    for segment in 0..10 {
        let hit = h
            .ingest(&app, envelope(&Segment::new(&id, segment), None))
            .await;
        assert_eq!(hit.status, StatusCode::OK, "segment {segment}");
        assert_eq!(hit.rate_limits(), None, "segment {segment}");
    }
    assert_eq!(h.replay(&app, &id).await.json()["segmentCount"], 10);
    let event = error_event(&id).to_string();
    let hit = h
        .ingest(
            &app,
            format!("{{}}\n{{\"type\":\"event\"}}\n{event}\n").into_bytes(),
        )
        .await;
    assert_eq!(hit.status, StatusCode::OK);
    assert!(hit.json()["id"].is_string(), "{}", hit.json());
}

async fn ip_refusal_before_the_body_names_both_categories(mut config: Config) {
    config.ingest.error_rate_ip_per_min = 1;
    config.ingest.error_rate_ip_burst = 1;
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let event = error_event(&replay_id()).to_string();
    let body = format!("{{}}\n{{\"type\":\"event\"}}\n{event}\n").into_bytes();
    assert_eq!(h.ingest(&app, body.clone()).await.status, StatusCode::OK);
    let refused = h.ingest(&app, body).await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    let limits = refused.rate_limits().unwrap();
    assert!(limits.ends_with(":error;replay:ip"), "{limits}");
}

async fn body_limits(config: Config) {
    let h = Harness::new(config.clone()).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    let mut large = Segment::new(&id, 0);
    let mut padded = b"[\"".to_vec();
    padded.extend(std::iter::repeat_n(b'a', 2 * 1024 * 1024));
    padded.extend_from_slice(b"\"]");
    large.recording = padded;
    let hit = h.ingest(&app, envelope(&large, None)).await;
    assert_eq!(
        hit.status,
        StatusCode::OK,
        "a replay item may pass the event limit"
    );
    assert_eq!(h.replay(&app, &id).await.json()["segmentCount"], 1);

    let mut event = error_event(&id);
    event["extra"] = json!({ "pad": "x".repeat(config.ingest.error_max_body_bytes) });
    let hit = h
        .ingest(
            &app,
            format!("{{}}\n{{\"type\":\"event\"}}\n{event}\n").into_bytes(),
        )
        .await;
    assert_eq!(hit.status, StatusCode::PAYLOAD_TOO_LARGE);

    let huge = vec![b' '; config.ingest.replay_max_body_bytes + 1];
    assert_eq!(
        h.ingest(&app, huge).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

async fn routes_are_scoped_to_the_app_and_workspace(config: Config) {
    let h = Harness::new(config).await;
    let a = h.error_app(true).await;
    let b = h.error_app(true).await;
    let id = replay_id();
    h.ingest(&a, envelope(&Segment::new(&id, 0), None)).await;
    // App B's key cannot write into app A's replay.
    h.ingest(&b, envelope(&Segment::new(&id, 1), None)).await;
    assert_eq!(h.replay(&a, &id).await.json()["segments"], json!([0]));
    assert_eq!(h.replay(&b, &id).await.json()["segments"], json!([1]));
    let other = replay_id();
    h.ingest(&a, envelope(&Segment::new(&other, 0), None)).await;
    for path in [
        format!("/api/error-apps/{}/replays/{other}", b.id),
        format!("/api/error-apps/{}/replays/{other}/segments/0", b.id),
    ] {
        assert_eq!(
            h.as_editor("GET", &path, None).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        h.as_editor(
            "DELETE",
            &format!("/api/error-apps/{}/replays/{other}", b.id),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );

    let stranger = editor_session(&h.state).await.unwrap();
    for path in [
        format!("/api/error-apps/{}/replays", a.id),
        format!("/api/error-apps/{}/replays/{other}", a.id),
        format!("/api/error-apps/{}/replays/{other}/segments/0", a.id),
    ] {
        let hit = h
            .send("GET", &path, Vec::new(), &Harness::cookie(&stranger))
            .await;
        assert_eq!(hit.status, StatusCode::NOT_FOUND, "{path}");
    }

    let member = member_session(&h.state, h.editor.workspace_id)
        .await
        .unwrap();
    let member_headers = Harness::cookie(&member);
    let path = format!("/api/error-apps/{}/replays/{other}", a.id);
    assert_eq!(
        h.send("GET", &path, Vec::new(), &member_headers)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        h.send("DELETE", &path, Vec::new(), &member_headers)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let settings = h
        .send(
            "PUT",
            &format!("/api/error-apps/{}/replays-settings", a.id),
            json!({ "enabled": false, "dailyBytes": 1_048_576, "hourlyLimit": 1 })
                .to_string()
                .into_bytes(),
            &member_headers,
        )
        .await;
    assert_eq!(settings.status, StatusCode::FORBIDDEN);

    let deleted = h.as_editor("DELETE", &path, None).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert!(deleted.bytes.is_empty());
    assert_eq!(h.replay(&a, &other).await.status, StatusCode::NOT_FOUND);
}

async fn api_token_scopes(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    h.ingest(&app, envelope(&Segment::new(&id, 0), None)).await;
    let mut tokens = Vec::new();
    for scopes in [json!(["errors:read"]), json!(["replays:read"])] {
        let created = h
            .as_editor(
                "POST",
                "/api/api-tokens",
                Some(json!({ "name": "Replay", "scopes": scopes, "expires_in_days": null })),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
        tokens.push(created.json()["token"].as_str().unwrap().to_owned());
    }
    let bearer = |token: &str| vec![("authorization", format!("Bearer {token}"))];
    let list = format!("/api/error-apps/{}/replays", app.id);
    let detail = format!("/api/error-apps/{}/replays/{id}", app.id);
    let segment = format!("/api/error-apps/{}/replays/{id}/segments/0", app.id);

    let errors_read = bearer(&tokens[0]);
    assert_eq!(
        h.send("GET", &list, Vec::new(), &errors_read).await.status,
        StatusCode::OK
    );
    assert_eq!(
        h.send("GET", &detail, Vec::new(), &errors_read)
            .await
            .status,
        StatusCode::OK
    );
    let refused = h.send("GET", &segment, Vec::new(), &errors_read).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.json()["code"], "missing_scope");

    let replays_read = bearer(&tokens[1]);
    assert_eq!(
        h.send("GET", &segment, Vec::new(), &replays_read)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        h.send("GET", &list, Vec::new(), &replays_read).await.status,
        StatusCode::FORBIDDEN
    );
}

async fn errors_link_to_replays(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    let hit = h
        .ingest(
            &app,
            envelope(&Segment::new(&id, 0), Some(error_event(&id))),
        )
        .await;
    assert_eq!(hit.status, StatusCode::OK);
    let event_id = hit.json()["id"].as_str().unwrap().to_owned();
    let event = h
        .as_editor("GET", &format!("/api/error-events/{event_id}"), None)
        .await
        .json();
    let replay = h.replay(&app, &id).await.json();
    assert_eq!(
        event["replay"],
        json!({ "id": id, "startedMs": replay["startedMs"] })
    );
    assert_eq!(replay["errorCount"], 1);
    assert_eq!(replay["errors"][0]["eventId"], event_id);
    let issue_id = replay["errors"][0]["issueId"].as_str().unwrap();
    let issue = h
        .as_editor("GET", &format!("/api/error-issues/{issue_id}"), None)
        .await
        .json();
    assert_eq!(issue["replayEventCount"], 1);
    // The issue page shows its newest event from `latest`, so it needs the link too.
    assert_eq!(issue["latest"]["replay"], event["replay"]);

    // A replay that was never stored, or that is in another app, is not linked.
    let missing = replay_id();
    let lone = error_event(&missing).to_string();
    let hit = h
        .ingest(
            &app,
            format!("{{}}\n{{\"type\":\"event\"}}\n{lone}\n").into_bytes(),
        )
        .await;
    let lone_id = hit.json()["id"].as_str().unwrap().to_owned();
    let event = h
        .as_editor("GET", &format!("/api/error-events/{lone_id}"), None)
        .await
        .json();
    assert_eq!(event["replay"], Value::Null);
}

async fn list_filters_and_pages(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let mut ids = Vec::new();
    for index in 0..3 {
        let id = replay_id();
        let url = format!("https://shop.example/page-{index}");
        let mut segment = Segment::new(&id, 0);
        segment.url = &url;
        let event = (index == 1).then(|| error_event(&id));
        h.ingest(&app, envelope(&segment, event)).await;
        ids.push(id);
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    let list = |query: &str| format!("/api/error-apps/{}/replays?{query}", app.id);
    let page = h.as_editor("GET", &list("limit=2"), None).await.json();
    assert_eq!(page["items"][0]["id"], ids[2]);
    assert_eq!(page["items"][1]["id"], ids[1]);
    let cursor = page["nextCursor"].as_str().unwrap();
    let rest = h
        .as_editor("GET", &list(&format!("limit=2&cursor={cursor}")), None)
        .await
        .json();
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert_eq!(rest["items"][0]["id"], ids[0]);
    assert_eq!(rest["nextCursor"], Value::Null);

    let by_url = h.as_editor("GET", &list("url=PAGE-2"), None).await.json();
    assert_eq!(by_url["items"].as_array().unwrap().len(), 1);
    assert_eq!(by_url["items"][0]["id"], ids[2]);
    let with_errors = h
        .as_editor("GET", &list("hasErrors=true"), None)
        .await
        .json();
    assert_eq!(with_errors["items"].as_array().unwrap().len(), 1);
    assert_eq!(with_errors["items"][0]["id"], ids[1]);
    let none = h
        .as_editor("GET", &list("release=other&environment=production"), None)
        .await
        .json();
    assert_eq!(none["items"], json!([]));
    let all = h
        .as_editor(
            "GET",
            &list("release=shop%402.0&environment=production"),
            None,
        )
        .await
        .json();
    assert_eq!(all["items"].as_array().unwrap().len(), 3);
}

async fn settings_are_validated(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(false).await;
    for (body, field) in [
        (
            json!({ "enabled": true, "dailyBytes": 1_048_575, "hourlyLimit": 10 }),
            "daily_bytes",
        ),
        (
            json!({ "enabled": true, "dailyBytes": 107_374_182_401_i64, "hourlyLimit": 10 }),
            "daily_bytes",
        ),
        (
            json!({ "enabled": true, "dailyBytes": 1_048_576, "hourlyLimit": 0 }),
            "hourly_limit",
        ),
        (
            json!({ "enabled": true, "dailyBytes": 1_048_576, "hourlyLimit": 100_001 }),
            "hourly_limit",
        ),
    ] {
        let hit = h.settings(&app, body).await;
        assert_eq!(hit.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(hit.json()["fields"][field].is_string(), "{}", hit.json());
    }
    let max = h
        .settings(
            &app,
            json!({ "enabled": true, "dailyBytes": 107_374_182_400_i64, "hourlyLimit": 100_000 }),
        )
        .await;
    assert_eq!(max.status, StatusCode::OK);
    assert_eq!(max.json()["replays"]["enabled"], true);
}

async fn privacy_requests_find_replays(config: Config) {
    let h = Harness::new(config).await;
    let app = h.error_app(true).await;
    let id = replay_id();
    h.ingest(&app, envelope(&Segment::new(&id, 0), None)).await;
    let export = h
        .as_editor("GET", "/api/privacy/errors?user=Ada%40Example.com", None)
        .await;
    assert_eq!(export.status, StatusCode::OK);
    let replays = &export.json()["replays"];
    assert_eq!(replays.as_array().unwrap().len(), 1);
    assert_eq!(replays[0]["id"], id);
    assert_eq!(replays[0]["appId"], app.id);
    assert_eq!(
        replays[0]["segments"],
        json!([format!(
            "/api/error-apps/{}/replays/{id}/segments/0",
            app.id
        )])
    );
    let other_user = h
        .as_editor("GET", "/api/privacy/errors?user=203.0.113.9", None)
        .await;
    assert_eq!(
        other_user.json()["replays"],
        json!([]),
        "no IP hash is kept"
    );

    let deleted = h
        .as_editor("DELETE", "/api/privacy/errors?user=42", None)
        .await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(h.replay(&app, &id).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sqlite_replays() {
    exercise(sqlite_config()).await;
}

#[tokio::test]
async fn postgres_replays() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| url.starts_with("postgres"))
    else {
        return;
    };
    let mut config = sqlite_config();
    config.database_url = Some(url);
    exercise(config).await;
}
