//! OTLP/HTTP intake, telemetry sources, log search, live tail, and retention over HTTP.

use std::io::Write;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use flate2::Compression;
use flate2::write::GzEncoder;
use http_body_util::BodyExt;
use owlee_platform::Config;
use owlee_server::{
    AppState, TestSession, boot, editor_session, member_session, prune_logs, router, testing_config,
};
use owlee_telemetry::prost::Message;
use owlee_telemetry::proto::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use owlee_telemetry::proto::collector::trace::v1::ExportTraceServiceResponse;
use owlee_telemetry::proto::common::v1::any_value::Value as Any;
use owlee_telemetry::proto::common::v1::{AnyValue, KeyValue};
use owlee_telemetry::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use owlee_telemetry::proto::resource::v1::Resource;
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;

const PROTOBUF: &str = "application/x-protobuf";
const JSON: &str = "application/json";
const DAY_MS: i64 = 86_400_000;

struct Hit {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}

impl Hit {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }
}

struct Harness {
    app: Router,
    state: AppState,
    editor: TestSession,
}

struct Source {
    id: String,
    token: String,
}

/// One log record: severity number, body, attributes, and time in Unix milliseconds.
struct Record {
    severity: i32,
    body: &'static str,
    attributes: Vec<(&'static str, Any)>,
    time_ms: i64,
    trace_id: Vec<u8>,
}

impl Record {
    fn new(severity: i32, body: &'static str) -> Self {
        Self {
            severity,
            body,
            attributes: Vec::new(),
            time_ms: now_ms(),
            trace_id: Vec::new(),
        }
    }

    fn attribute(mut self, key: &'static str, value: Any) -> Self {
        self.attributes.push((key, value));
        self
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn text(value: &str) -> Any {
    Any::StringValue(value.to_owned())
}

fn pair(key: &str, value: Any) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue { value: Some(value) }),
        ..KeyValue::default()
    }
}

/// A protobuf `ExportLogsServiceRequest` built with the generated types, as an SDK sends it.
fn protobuf_logs(service: &str, records: Vec<Record>) -> Vec<u8> {
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![
                    pair("service.name", text(service)),
                    pair("deployment.environment.name", text("production")),
                    pair("host.name", text("web-1")),
                ],
                ..Resource::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: records
                    .into_iter()
                    .map(|record| LogRecord {
                        time_unix_nano: u64::try_from(record.time_ms).unwrap() * 1_000_000,
                        severity_number: record.severity,
                        body: Some(AnyValue {
                            value: Some(text(record.body)),
                        }),
                        attributes: record
                            .attributes
                            .into_iter()
                            .map(|(key, value)| pair(key, value))
                            .collect(),
                        trace_id: record.trace_id,
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

/// The logs example of the OTLP specification, written by hand, with the time set to `time_ms`.
fn json_logs(time_ms: i64) -> Vec<u8> {
    json!({
      "resourceLogs": [{
        "resource": { "attributes": [
          { "key": "service.name", "value": { "stringValue": "my.service" } },
          { "key": "client.address", "value": { "stringValue": "203.0.113.9" } }
        ] },
        "scopeLogs": [{
          "scope": { "name": "my.library", "version": "1.0.0" },
          "logRecords": [{
            "timeUnixNano": format!("{}", time_ms * 1_000_000),
            "observedTimeUnixNano": format!("{}", time_ms * 1_000_000),
            "severityNumber": 10,
            "severityText": "Information",
            "traceId": "5B8EFFF798038103D269B633813FC60C",
            "spanId": "EEE19B7EC3C1B174",
            "body": { "stringValue": "Example log record" },
            "attributes": [
              { "key": "string.attribute", "value": { "stringValue": "some string" } },
              { "key": "int.attribute", "value": { "intValue": "10" } },
              { "key": "password", "value": { "stringValue": "hunter2" } }
            ]
          }]
        }]
      }]
    })
    .to_string()
    .into_bytes()
}

fn gzip(body: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
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

    async fn api(
        &self,
        session: &TestSession,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> Hit {
        let headers = vec![
            ("cookie", session.cookie.clone()),
            ("x-owlee-workspace", session.workspace_id.to_string()),
            ("content-type", JSON.to_owned()),
            ("origin", "http://127.0.0.1".to_owned()),
        ];
        let body = body
            .map(|body| body.to_string().into_bytes())
            .unwrap_or_default();
        self.send(method, uri, body, &headers).await
    }

    async fn editor(&self, method: &str, uri: &str, body: Option<Value>) -> Hit {
        self.api(&self.editor, method, uri, body).await
    }

    async fn source(&self, name: &str) -> Source {
        let created = self
            .editor(
                "POST",
                "/api/telemetry-sources",
                Some(json!({ "project_id": self.editor.project_id, "name": name })),
            )
            .await;
        assert_eq!(created.status, StatusCode::OK, "{:?}", created.json());
        let body = created.json();
        let token = body["token"].as_str().unwrap().to_owned();
        assert!(token.starts_with("otlp_"));
        assert_eq!(body["tokenHint"], token[token.len() - 4..]);
        Source {
            id: body["id"].as_str().unwrap().to_owned(),
            token,
        }
    }

    async fn otlp(&self, signal: &str, token: &str, content_type: &str, body: Vec<u8>) -> Hit {
        self.send(
            "POST",
            &format!("/otlp/v1/{signal}"),
            body,
            &[
                ("authorization", format!("Bearer {token}")),
                ("content-type", content_type.to_owned()),
            ],
        )
        .await
    }

    async fn logs(&self, query: &str) -> Hit {
        self.editor(
            "GET",
            &format!("/api/logs?project={}&{query}", self.editor.project_id),
            None,
        )
        .await
    }

    async fn bodies(&self, q: &str) -> Vec<String> {
        let hit = self.logs(&format!("q={}", urlencode(q))).await;
        assert_eq!(hit.status, StatusCode::OK, "{q}: {:?}", hit.json());
        hit.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["body"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn count(&self, sql: &str, id: &str) -> i64 {
        sqlx::query(sql)
            .bind(id)
            .fetch_one(self.state.db.pool())
            .await
            .unwrap()
            .try_get::<i64, _>(0)
            .unwrap()
    }

    async fn source_view(&self, id: &str) -> Value {
        let list = self
            .editor(
                "GET",
                &format!("/api/telemetry-sources?project={}", self.editor.project_id),
                None,
            )
            .await;
        assert_eq!(list.status, StatusCode::OK);
        list.json()
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["id"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    }
}

fn urlencode(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn sqlite_config() -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    testing_config(path)
}

async fn exercise(config: Config) {
    stores_protobuf_json_and_gzip(config.clone()).await;
    refuses_bad_tokens_bodies_and_types(config.clone()).await;
    other_workspaces_and_members(config.clone()).await;
    quota_gives_partial_success_then_429(config.clone()).await;
    search_filters(config.clone()).await;
    live_tail_returns_only_newer_rows(config.clone()).await;
    traces_and_metrics_are_rejected_in_partial_success(config.clone()).await;
    retention_and_source_deletion(config.clone()).await;
    sources_are_validated_and_rotated(config).await;
}

async fn stores_protobuf_json_and_gzip(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    let view = h.source_view(&source.id).await;
    assert_eq!(view["firstLogAt"], Value::Null);
    assert_eq!(view["dailyQuota"], Value::Null);
    assert_eq!(
        view["intake"],
        json!({
            "logs": { "stored": 0, "dropped": 0, "last_drop": null },
            "traces": { "stored": 0, "dropped": 0, "last_drop": null },
            "metrics": { "stored": 0, "dropped": 0, "last_drop": null },
        })
    );

    let time_ms = now_ms() - 1000;
    let mut record = Record::new(17, "payment failed")
        .attribute("http.route", text("/checkout"))
        .attribute("http.status_code", Any::IntValue(502));
    record.time_ms = time_ms;
    record.trace_id = (1..=16).collect();
    let stored = h
        .otlp(
            "logs",
            &source.token,
            PROTOBUF,
            protobuf_logs("checkout", vec![record]),
        )
        .await;
    assert_eq!(stored.status, StatusCode::OK);
    assert_eq!(stored.headers["content-type"], PROTOBUF);
    assert_eq!(
        ExportLogsServiceResponse::decode(stored.bytes.as_slice()).unwrap(),
        ExportLogsServiceResponse::default()
    );

    let json_time = now_ms() - 2000;
    let stored = h
        .otlp("logs", &source.token, JSON, json_logs(json_time))
        .await;
    assert_eq!(stored.status, StatusCode::OK);
    assert_eq!(stored.headers["content-type"], JSON);
    assert_eq!(stored.json(), json!({}));

    let zipped = h
        .send(
            "POST",
            "/otlp/v1/logs",
            gzip(&protobuf_logs("worker", vec![Record::new(9, "gzip body")])),
            &[
                ("authorization", format!("bearer {}", source.token)),
                ("content-type", PROTOBUF.to_owned()),
                ("content-encoding", "gzip".to_owned()),
            ],
        )
        .await;
    assert_eq!(zipped.status, StatusCode::OK);

    let page = h.logs("").await;
    assert_eq!(page.status, StatusCode::OK);
    let page = page.json();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(page["nextCursor"], Value::Null);
    assert!(page["tailCursor"].as_str().is_some());
    let first = items
        .iter()
        .find(|item| item["service"] == "checkout")
        .unwrap();
    let id = first["id"].as_str().unwrap().to_owned();
    assert_eq!(
        first,
        &json!({
            "id": id,
            "timeMs": time_ms,
            "level": "error",
            "severityText": "",
            "service": "checkout",
            "environment": "production",
            "host": "web-1",
            "body": "payment failed",
            "traceId": "0102030405060708090a0b0c0d0e0f10",
            "spanId": null,
            "sourceId": source.id,
            "sourceName": "api",
        })
    );
    // Newest first.
    assert_eq!(items[0]["body"], "gzip body");

    let detail = h.editor("GET", &format!("/api/logs/{id}"), None).await;
    assert_eq!(detail.status, StatusCode::OK);
    let detail = detail.json();
    assert_eq!(
        detail["attributes"],
        json!({ "http.route": "/checkout", "http.status_code": 502 })
    );
    assert_eq!(
        detail["resource"],
        json!({ "deployment.environment.name": "production", "host.name": "web-1", "service.name": "checkout" })
    );

    let from_json = items
        .iter()
        .find(|item| item["service"] == "my.service")
        .unwrap();
    assert_eq!(from_json["timeMs"], json_time);
    assert_eq!(from_json["level"], "info");
    assert_eq!(from_json["traceId"], "5b8efff798038103d269b633813fc60c");
    assert_eq!(from_json["spanId"], "eee19b7ec3c1b174");
    let detail = h
        .editor(
            "GET",
            &format!("/api/logs/{}", from_json["id"].as_str().unwrap()),
            None,
        )
        .await
        .json();
    assert_eq!(
        detail["attributes"],
        json!({ "int.attribute": 10, "password": "[Filtered]", "string.attribute": "some string" })
    );
    assert_eq!(detail["resource"], json!({ "service.name": "my.service" }));

    let view = h.source_view(&source.id).await;
    assert_eq!(view["intake"]["logs"]["stored"], 3);
    assert!(view["firstLogAt"].as_str().is_some());
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM log_attributes a JOIN logs l ON l.id = a.log_id WHERE l.source_id = $1",
            &source.id
        )
        .await,
        5
    );
}

async fn refuses_bad_tokens_bodies_and_types(mut config: Config) {
    config.ingest.otlp_max_body_bytes = 2048;
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    let body = protobuf_logs("api", vec![Record::new(9, "hello")]);

    let wrong = h.otlp("logs", "otlp_wrong", PROTOBUF, body.clone()).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let missing = h
        .send(
            "POST",
            "/otlp/v1/logs",
            body.clone(),
            &[("content-type", PROTOBUF.to_owned())],
        )
        .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);

    let bad = h
        .otlp("logs", &source.token, PROTOBUF, vec![0xff; 16])
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let bad = h
        .otlp(
            "logs",
            &source.token,
            JSON,
            b"{\"resourceLogs\": 5}".to_vec(),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.json()["code"], 3);
    let bad_gzip = h
        .send(
            "POST",
            "/otlp/v1/logs",
            b"not gzip".to_vec(),
            &[
                ("authorization", format!("Bearer {}", source.token)),
                ("content-type", PROTOBUF.to_owned()),
                ("content-encoding", "gzip".to_owned()),
            ],
        )
        .await;
    assert_eq!(bad_gzip.status, StatusCode::BAD_REQUEST);

    let text_type = h.otlp("logs", &source.token, "text/plain", body).await;
    assert_eq!(text_type.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let large = h.otlp("logs", &source.token, JSON, vec![b' '; 4096]).await;
    assert_eq!(large.status, StatusCode::PAYLOAD_TOO_LARGE);
    // A small gzip body that inflates past the limit.
    let bomb = h
        .send(
            "POST",
            "/otlp/v1/logs",
            gzip(&vec![b' '; 64 * 1024]),
            &[
                ("authorization", format!("Bearer {}", source.token)),
                ("content-type", JSON.to_owned()),
                ("content-encoding", "gzip".to_owned()),
            ],
        )
        .await;
    assert_eq!(bomb.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM logs WHERE source_id = $1", &source.id)
            .await,
        0
    );
}

async fn other_workspaces_and_members(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    h.otlp(
        "logs",
        &source.token,
        PROTOBUF,
        protobuf_logs("api", vec![Record::new(9, "mine")]),
    )
    .await;
    let id = h.logs("").await.json()["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let stranger = editor_session(&h.state).await.unwrap();
    for uri in [
        format!("/api/logs/{id}"),
        format!("/api/logs?project={}", h.editor.project_id),
        format!("/api/logs/facets?project={}", h.editor.project_id),
        format!("/api/telemetry-sources?project={}", h.editor.project_id),
    ] {
        assert_eq!(
            h.api(&stranger, "GET", &uri, None).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let theirs = h.api(&stranger, "GET", "/api/logs", None).await.json();
    assert_eq!(theirs["items"], json!([]));
    let theirs = h
        .api(&stranger, "GET", "/api/telemetry-sources", None)
        .await
        .json();
    assert_eq!(theirs, json!([]));
    for (method, uri, body) in [
        (
            "PATCH",
            format!("/api/telemetry-sources/{}", source.id),
            Some(json!({ "name": "x" })),
        ),
        (
            "DELETE",
            format!("/api/telemetry-sources/{}", source.id),
            None,
        ),
        (
            "POST",
            format!("/api/telemetry-sources/{}/rotate", source.id),
            None,
        ),
    ] {
        assert_eq!(
            h.api(&stranger, method, &uri, body).await.status,
            StatusCode::NOT_FOUND,
            "{method} {uri}"
        );
    }
    let created = h
        .api(
            &stranger,
            "POST",
            "/api/telemetry-sources",
            Some(json!({ "project_id": h.editor.project_id, "name": "intruder" })),
        )
        .await;
    assert_eq!(created.status, StatusCode::NOT_FOUND);

    let member = member_session(&h.state, h.editor.workspace_id)
        .await
        .unwrap();
    let listed = h
        .api(
            &member,
            "GET",
            &format!("/api/telemetry-sources?project={}", h.editor.project_id),
            None,
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.json()[0]["id"], source.id);
    assert_eq!(listed.json()[0].get("token"), None);
    assert_eq!(
        h.api(&member, "GET", &format!("/api/logs/{id}"), None)
            .await
            .status,
        StatusCode::OK
    );
    let created = h
        .api(
            &member,
            "POST",
            "/api/telemetry-sources",
            Some(json!({ "project_id": h.editor.project_id, "name": "member" })),
        )
        .await;
    assert_eq!(created.status, StatusCode::FORBIDDEN);
    assert_eq!(
        h.api(
            &member,
            "DELETE",
            &format!("/api/telemetry-sources/{}", source.id),
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
}

async fn quota_gives_partial_success_then_429(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    let patched = h
        .editor(
            "PATCH",
            &format!("/api/telemetry-sources/{}", source.id),
            Some(json!({ "dailyQuota": 3 })),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK);
    assert_eq!(patched.json()["dailyQuota"], 3);
    let five = (0..5).map(|_| Record::new(9, "line")).collect();
    let partial = h
        .otlp("logs", &source.token, PROTOBUF, protobuf_logs("api", five))
        .await;
    assert_eq!(partial.status, StatusCode::OK);
    let response = ExportLogsServiceResponse::decode(partial.bytes.as_slice()).unwrap();
    let success = response.partial_success.unwrap();
    assert_eq!(success.rejected_log_records, 2);
    assert_eq!(success.error_message, "daily log quota reached");

    let full = h
        .otlp("logs", &source.token, JSON, json_logs(now_ms()))
        .await;
    assert_eq!(full.status, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = full.headers["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=86_400).contains(&retry));
    assert_eq!(full.json()["code"], 8);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM logs WHERE source_id = $1", &source.id)
            .await,
        3
    );
    let intake = &h.source_view(&source.id).await["intake"]["logs"];
    assert_eq!(intake["stored"], 3);
    assert_eq!(intake["dropped"], 3);
    assert_eq!(intake["last_drop"]["reason"], "daily_quota");

    // `null` goes back to the instance default of 1,000,000.
    let patched = h
        .editor(
            "PATCH",
            &format!("/api/telemetry-sources/{}", source.id),
            Some(json!({ "dailyQuota": null })),
        )
        .await;
    assert_eq!(patched.json()["dailyQuota"], Value::Null);
    let again = h
        .otlp("logs", &source.token, JSON, json_logs(now_ms()))
        .await;
    assert_eq!(again.status, StatusCode::OK);
}

async fn search_filters(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    let trace: Vec<u8> = vec![0xab; 16];
    let mut traced = Record::new(9, "Checkout started").attribute("http.route", text("/checkout"));
    traced.trace_id = trace;
    let api = vec![
        traced,
        Record::new(13, "slow query took 900ms")
            .attribute("db.system", text("postgres"))
            .attribute("retry", Any::BoolValue(true)),
        Record::new(17, "connection RESET by peer").attribute("http.route", text("/pay")),
        Record::new(21, "out of memory"),
        Record::new(5, "100% done"),
    ];
    h.otlp("logs", &source.token, PROTOBUF, protobuf_logs("api", api))
        .await;
    h.otlp(
        "logs",
        &source.token,
        PROTOBUF,
        protobuf_logs(
            "worker",
            vec![
                Record::new(17, "job failed: timeout"),
                Record::new(9, "100 jobs done"),
            ],
        ),
    )
    .await;

    let sorted = |mut bodies: Vec<String>| {
        bodies.sort();
        bodies
    };
    assert_eq!(h.bodies("").await.len(), 7);
    assert_eq!(
        sorted(h.bodies("service:worker").await),
        vec!["100 jobs done", "job failed: timeout"]
    );
    assert_eq!(
        sorted(h.bodies("level:>=warn").await),
        vec![
            "connection RESET by peer",
            "job failed: timeout",
            "out of memory",
            "slow query took 900ms"
        ]
    );
    assert_eq!(
        sorted(h.bodies("level:error").await),
        vec!["connection RESET by peer", "job failed: timeout"]
    );
    assert_eq!(
        h.bodies("http.route:/checkout").await,
        vec!["Checkout started"]
    );
    assert_eq!(h.bodies("retry:true").await, vec!["slow query took 900ms"]);
    assert_eq!(h.bodies("reset").await, vec!["connection RESET by peer"]);
    assert_eq!(
        h.bodies("\"connection reset\"").await,
        vec!["connection RESET by peer"]
    );
    // `%` is a literal, not a wildcard.
    assert_eq!(h.bodies("100%").await, vec!["100% done"]);
    assert_eq!(h.bodies("_").await, Vec::<String>::new());
    assert_eq!(
        sorted(h.bodies("level:>=error -service:worker").await),
        vec!["connection RESET by peer", "out of memory"]
    );
    assert_eq!(
        sorted(
            h.bodies("-http.route:/checkout service:api level:>=info")
                .await
        ),
        vec![
            "connection RESET by peer",
            "out of memory",
            "slow query took 900ms"
        ]
    );
    let trace_id = "ab".repeat(16);
    assert_eq!(
        h.bodies(&format!("trace:{trace_id}")).await,
        vec!["Checkout started"]
    );
    assert_eq!(h.bodies(&format!("-trace:{trace_id}")).await.len(), 6);
    assert_eq!(
        h.bodies("env:production host:web-1 service:api")
            .await
            .len(),
        5
    );
    assert_eq!(h.bodies("env:dev").await, Vec::<String>::new());

    let bad = h.logs("q=level%3Aloud").await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        bad.json()["fields"],
        json!({ "q": "Use a level of trace, debug, info, warn, error, or fatal, optionally after `>=` in `level:loud`." })
    );
    let now = now_ms();
    let wide = h.logs(&format!("from={}&to={now}", now - 8 * DAY_MS)).await;
    assert_eq!(wide.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        wide.json()["fields"]["to"],
        "Use a time range of at most 7 days."
    );
    let old = h
        .logs(&format!(
            "from={}&to={}",
            now - 3 * DAY_MS,
            now - 2 * DAY_MS
        ))
        .await;
    assert_eq!(old.json()["items"], json!([]));

    let first = h.logs("limit=4").await.json();
    assert_eq!(first["items"].as_array().unwrap().len(), 4);
    let cursor = first["nextCursor"].as_str().unwrap();
    let second = h.logs(&format!("limit=4&cursor={cursor}")).await.json();
    assert_eq!(second["items"].as_array().unwrap().len(), 3);
    assert_eq!(second["nextCursor"], Value::Null);
    let mut ids: Vec<&str> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap())
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    ids.dedup();
    assert_eq!(ids.len(), 7);

    let facets = h
        .editor(
            "GET",
            &format!(
                "/api/logs/facets?project={}&q=-level%3Adebug",
                h.editor.project_id
            ),
            None,
        )
        .await;
    assert_eq!(facets.status, StatusCode::OK);
    assert_eq!(
        facets.json(),
        json!({
            "levels": [
                { "level": "trace", "count": 0 },
                { "level": "debug", "count": 0 },
                { "level": "info", "count": 2 },
                { "level": "warn", "count": 1 },
                { "level": "error", "count": 2 },
                { "level": "fatal", "count": 1 },
            ],
            "services": [
                { "service": "api", "count": 4 },
                { "service": "worker", "count": 2 },
            ],
            "capped": false,
        })
    );
    let bad = h.editor("GET", "/api/logs/facets?q=%22open", None).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.json()["fields"]["q"], "Close the quote in `\"open`.");
}

async fn live_tail_returns_only_newer_rows(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    h.otlp(
        "logs",
        &source.token,
        PROTOBUF,
        protobuf_logs("api", vec![Record::new(9, "before")]),
    )
    .await;
    let page = h.logs("").await.json();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let cursor = page["tailCursor"].as_str().unwrap().to_owned();
    let empty = h.logs(&format!("after={cursor}")).await.json();
    assert_eq!(empty["items"], json!([]));
    assert_eq!(empty["tailCursor"], cursor);

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    h.otlp(
        "logs",
        &source.token,
        PROTOBUF,
        protobuf_logs(
            "api",
            vec![Record::new(9, "after one"), Record::new(17, "after two")],
        ),
    )
    .await;
    let tail = h.logs(&format!("after={cursor}")).await;
    assert_eq!(tail.status, StatusCode::OK);
    let tail = tail.json();
    let mut bodies: Vec<&str> = tail["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["body"].as_str().unwrap())
        .collect();
    bodies.sort_unstable();
    assert_eq!(bodies, vec!["after one", "after two"]);
    assert_eq!(tail["nextCursor"], Value::Null);
    let next = tail["tailCursor"].as_str().unwrap();
    assert!(next > cursor.as_str());
    let filtered = h
        .logs(&format!("after={cursor}&q=level%3Aerror"))
        .await
        .json();
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        h.logs(&format!("after={next}")).await.json()["items"],
        json!([])
    );
    assert_eq!(h.logs("after=nope").await.status, StatusCode::BAD_REQUEST);
}

async fn traces_and_metrics_are_rejected_in_partial_success(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    let traces = br#"{"resourceSpans":[{"scopeSpans":[{"spans":[
        {"traceId":"5b8efff798038103d269b633813fc60c","spanId":"eee19b7ec3c1b174","name":"GET /"},
        {"traceId":"5b8efff798038103d269b633813fc60c","spanId":"eee19b7ec3c1b175","name":"db"}]}]}]}"#;
    let hit = h.otlp("traces", &source.token, JSON, traces.to_vec()).await;
    assert_eq!(hit.status, StatusCode::OK);
    assert_eq!(
        hit.json(),
        json!({ "partialSuccess": { "rejectedSpans": "2", "errorMessage": "traces are not stored yet" } })
    );
    let empty = h.otlp("traces", &source.token, PROTOBUF, Vec::new()).await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(
        ExportTraceServiceResponse::decode(empty.bytes.as_slice()).unwrap(),
        ExportTraceServiceResponse::default()
    );
    let metrics = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
        {"name":"cpu","gauge":{"dataPoints":[{"asDouble":0.5,"timeUnixNano":"1"}]}}]}]}]}"#;
    let hit = h
        .otlp("metrics", &source.token, JSON, metrics.to_vec())
        .await;
    assert_eq!(
        hit.json(),
        json!({ "partialSuccess": { "rejectedDataPoints": "1", "errorMessage": "metrics are not stored yet" } })
    );
    let wrong = h.otlp("metrics", "otlp_nope", JSON, metrics.to_vec()).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let intake = &h.source_view(&source.id).await["intake"];
    assert_eq!(intake["traces"]["dropped"], 2);
    assert_eq!(intake["traces"]["last_drop"]["reason"], "unsupported");
    assert_eq!(intake["metrics"]["dropped"], 1);
    assert_eq!(intake["logs"]["dropped"], 0);
}

async fn retention_and_source_deletion(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    let mut old = Record::new(9, "ten days old").attribute("k", text("v"));
    old.time_ms = now_ms() - 10 * DAY_MS;
    let fresh = Record::new(9, "fresh").attribute("k", text("v"));
    h.otlp(
        "logs",
        &source.token,
        PROTOBUF,
        protobuf_logs("api", vec![old, fresh]),
    )
    .await;
    let logs = "SELECT COUNT(*) FROM logs WHERE source_id = $1";
    let attributes = "SELECT COUNT(*) FROM log_attributes a JOIN logs l ON l.id = a.log_id WHERE l.source_id = $1";
    assert_eq!(h.count(logs, &source.id).await, 2);
    prune_logs(&h.state.db, now_ms() - 7 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(h.count(logs, &source.id).await, 1);
    assert_eq!(h.count(attributes, &source.id).await, 1);
    assert_eq!(h.bodies("").await, vec!["fresh"]);

    let deleted = h
        .editor(
            "DELETE",
            &format!("/api/telemetry-sources/{}", source.id),
            None,
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let refused = h
        .otlp(
            "logs",
            &source.token,
            PROTOBUF,
            protobuf_logs("api", vec![Record::new(9, "late")]),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(h.bodies("").await, Vec::<String>::new());
    assert_eq!(h.source_view(&source.id).await, Value::Null);
    // The name is free at once, while the old rows wait for the retention task.
    h.source("api").await;
    assert_eq!(h.count(logs, &source.id).await, 1);

    prune_logs(&h.state.db, now_ms() - 7 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(h.count(logs, &source.id).await, 0);
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM telemetry_sources WHERE id = $1",
            &source.id
        )
        .await,
        0
    );
}

async fn sources_are_validated_and_rotated(config: Config) {
    let h = Harness::new(config).await;
    let source = h.source("api").await;
    let uri = format!("/api/telemetry-sources/{}", source.id);
    let taken = h
        .editor(
            "POST",
            "/api/telemetry-sources",
            Some(json!({ "project_id": h.editor.project_id, "name": " api " })),
        )
        .await;
    assert_eq!(taken.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        taken.json()["fields"]["name"],
        "A source with this name exists in the project."
    );
    let blank = h.editor("PATCH", &uri, Some(json!({ "name": "  " }))).await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    let zero = h
        .editor("PATCH", &uri, Some(json!({ "dailyQuota": 0 })))
        .await;
    assert_eq!(zero.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(zero.json()["fields"]["dailyQuota"].is_string());
    let renamed = h
        .editor("PATCH", &uri, Some(json!({ "name": "edge" })))
        .await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert_eq!(renamed.json()["name"], "edge");
    assert_eq!(renamed.json()["dailyQuota"], Value::Null);

    let rotated = h.editor("POST", &format!("{uri}/rotate"), None).await;
    assert_eq!(rotated.status, StatusCode::OK);
    let token = rotated.json()["token"].as_str().unwrap().to_owned();
    assert_ne!(token, source.token);
    let body = || protobuf_logs("api", vec![Record::new(9, "x")]);
    assert_eq!(
        h.otlp("logs", &source.token, PROTOBUF, body()).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.otlp("logs", &token, PROTOBUF, body()).await.status,
        StatusCode::OK
    );
    assert_eq!(
        h.source_view(&source.id).await["tokenHint"],
        token[token.len() - 4..]
    );
    let stored_hash = sqlx::query("SELECT token_hash FROM telemetry_sources WHERE id = $1")
        .bind(&source.id)
        .fetch_one(h.state.db.pool())
        .await
        .unwrap()
        .try_get::<String, _>(0)
        .unwrap();
    assert_eq!(stored_hash, owlee_platform::hash_token(&token));
}

#[tokio::test]
async fn sqlite_telemetry() {
    exercise(sqlite_config()).await;
}

#[tokio::test]
async fn postgres_telemetry() {
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
