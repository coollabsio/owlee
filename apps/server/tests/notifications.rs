use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_platform::TimestampMillis;
use owlee_server::{
    AppState, TestSession, boot, editor_session, retry_deliveries, router, testing_config,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tower::ServiceExt;

/// Each scenario runs on SQLite, and on Postgres when `OWLEE_APP_POSTGRES_URL` is set.
macro_rules! on_both_backends {
    ($scenario:ident) => {
        mod $scenario {
            #[tokio::test]
            async fn sqlite() {
                super::$scenario(super::app(None).await).await;
            }

            #[tokio::test]
            async fn postgres() {
                let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
                    .ok()
                    .filter(|url| url.starts_with("postgres"))
                else {
                    return;
                };
                let url = super::fresh_postgres(&url).await;
                super::$scenario(super::app(Some(url)).await).await;
            }
        }
    };
}

on_both_backends!(new_and_regressed_issues_alert_once_each);
on_both_backends!(issue_alerts_are_capped_per_project);
on_both_backends!(a_failing_endpoint_is_retried_with_backoff_until_it_answers);
on_both_backends!(a_rejected_delivery_is_not_retried);

/// A new database on the server at `url`. The retry pass takes every due
/// delivery in the database, so scenarios cannot share one.
async fn fresh_postgres(url: &str) -> String {
    use sqlx::Connection;

    let name = format!(
        "owlee_test_{}",
        owlee_platform::Id::new().to_string().replace('-', "")
    );
    let mut admin = sqlx::PgConnection::connect(url).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut admin)
        .await
        .unwrap();
    let (server, _) = url.split('?').next().unwrap().rsplit_once('/').unwrap();
    format!("{server}/{name}")
}

struct Setup {
    app: Router,
    state: AppState,
    session: TestSession,
}

async fn app(postgres: Option<String>) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    let mut config = testing_config(path);
    config.database_url = postgres;
    config.ingest.error_rate_ip_burst = 1_000;
    config.ingest.error_rate_project_burst = 1_000;
    let state = boot(config).await.unwrap();
    let session = editor_session(&state).await.unwrap();
    Setup {
        app: router(state.clone()),
        state,
        session,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    session: Option<&TestSession>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-owlee-workspace", session.workspace_id.to_string());
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A webhook endpoint that answers with the queued statuses, then 204.
/// Every body it receives comes out of the channel.
async fn receiver(statuses: &[u16]) -> (String, mpsc::UnboundedReceiver<Value>) {
    let queue = Arc::new(Mutex::new(
        statuses.iter().copied().collect::<VecDeque<_>>(),
    ));
    let (tx, rx) = mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hook = Router::new().route(
        "/hook",
        axum::routing::post(move |body: String| {
            let queue = queue.clone();
            let tx = tx.clone();
            async move {
                let _ = tx.send(serde_json::from_str::<Value>(&body).unwrap());
                let next = queue.lock().unwrap().pop_front().unwrap_or(204);
                StatusCode::from_u16(next).unwrap()
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, hook).await.unwrap() });
    (format!("http://{addr}/hook"), rx)
}

async fn received(rx: &mut mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the webhook was called")
        .unwrap()
}

async fn add_channel(app: &Router, session: &TestSession, url: &str) -> String {
    let (status, channel) = call(
        app,
        "POST",
        "/api/notifications",
        Some(json!({
            "name": "Hooks", "provider": "webhook", "monitor_ids": [],
            "config": { "url": url, "secret": "" }
        })),
        Some(session),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{channel}");
    assert_eq!(channel["error_issues"], true);
    channel["id"].as_str().unwrap().to_owned()
}

/// The channel's deliveries, newest first, once `count` exist and none is
/// mid-attempt. An attempt in progress is `pending` without an error; one
/// waiting for a retry keeps the error of the last attempt.
async fn deliveries(app: &Router, session: &TestSession, count: usize) -> Vec<Value> {
    for _ in 0..250 {
        let (status, channels) =
            call(app, "GET", "/api/notifications", None, Some(session), &[]).await;
        assert_eq!(status, StatusCode::OK, "{channels}");
        let list = channels[0]["deliveries"].as_array().unwrap().clone();
        let in_flight = list
            .iter()
            .any(|delivery| delivery["status"] == "pending" && delivery["error"].is_null());
        if list.len() >= count && !in_flight {
            return list;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("deliveries did not settle");
}

/// The app's id and DSN key.
async fn create_error_app(app: &Router, session: &TestSession) -> (String, String) {
    let (status, created) = call(
        app,
        "POST",
        "/api/error-apps",
        Some(json!({"name": "Checkout", "platform": "php", "project_id": session.project_id})),
        Some(session),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    (
        created["id"].as_str().unwrap().to_owned(),
        created["publicKey"].as_str().unwrap().to_owned(),
    )
}

async fn send_error(app: &Router, (id, key): &(String, String), kind: &str) {
    let event = json!({
        "event_id": owlee_platform::Id::new().to_string().replace('-', ""),
        "level": "error",
        "platform": "php",
        "exception": {"values": [{
            "type": kind,
            "value": "disk full on volume 3",
            "stacktrace": {"frames": [{
                "filename": "/app/src/Store.php", "function": "save", "lineno": 42, "in_app": true
            }]}
        }]},
        "user": {"id": "9", "email": "ada@example.com"},
        "request": {"method": "POST", "url": "http://app/pay?card=4242"}
    });
    let (status, body) = call(
        app,
        "POST",
        &format!("/api/{id}/store/"),
        Some(event),
        None,
        &[(
            "x-sentry-auth",
            &format!("Sentry sentry_key={key}, sentry_version=7"),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn events(list: &[Value]) -> Vec<(&str, &str)> {
    list.iter()
        .map(|delivery| {
            (
                delivery["event"].as_str().unwrap(),
                delivery["status"].as_str().unwrap(),
            )
        })
        .collect()
}

async fn new_and_regressed_issues_alert_once_each(Setup { app, session, .. }: Setup) {
    let (url, mut rx) = receiver(&[]).await;
    add_channel(&app, &session, &url).await;
    let error_app = create_error_app(&app, &session).await;

    send_error(&app, &error_app, "RuntimeException").await;
    let alert = received(&mut rx).await;
    assert_eq!(alert["event"], "issue.new");
    let issue = &alert["issue"];
    assert_eq!(issue["short_id"], "CHECKOUT-1");
    assert_eq!(issue["project"], "Default");
    assert_eq!(issue["title"], "RuntimeException: disk full on volume 3");
    let issue_id = issue["id"].as_str().unwrap().to_owned();
    assert_eq!(
        issue["url"],
        format!(
            "http://127.0.0.1/errors/{issue_id}?workspace={}",
            session.workspace_id
        )
    );
    let text = alert.to_string();
    assert!(!text.contains("ada@example.com"), "{text}");
    assert!(!text.contains("4242"), "{text}");

    send_error(&app, &error_app, "RuntimeException").await;
    let (status, resolved) = call(
        &app,
        "PATCH",
        &format!("/api/error-issues/{issue_id}"),
        Some(json!({"status": "resolved"})),
        Some(&session),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    send_error(&app, &error_app, "RuntimeException").await;
    let alert = received(&mut rx).await;
    assert_eq!(alert["event"], "issue.regressed");
    assert_eq!(alert["issue"]["id"], json!(issue_id));

    let list = deliveries(&app, &session, 2).await;
    assert_eq!(
        events(&list),
        [("issue.regressed", "sent"), ("issue.new", "sent")]
    );
    assert!(rx.try_recv().is_err(), "no third alert");
}

async fn issue_alerts_are_capped_per_project(Setup { app, session, .. }: Setup) {
    let (url, mut rx) = receiver(&[]).await;
    add_channel(&app, &session, &url).await;
    let error_app = create_error_app(&app, &session).await;
    for n in 1..=12 {
        send_error(&app, &error_app, &format!("Error{n}")).await;
    }
    let mut short_ids = Vec::new();
    for _ in 0..10 {
        let alert = received(&mut rx).await;
        short_ids.push(alert["issue"]["short_id"].as_str().unwrap().to_owned());
    }
    short_ids.sort_by_key(|id| id["CHECKOUT-".len()..].parse::<u32>().unwrap());
    let expected: Vec<String> = (1..=10).map(|n| format!("CHECKOUT-{n}")).collect();
    assert_eq!(short_ids, expected);
    // The cap is checked inside the ingest request, so the last two never started.
    deliveries(&app, &session, 8).await;
    assert!(
        rx.try_recv().is_err(),
        "the 11th and 12th issues sent no alert"
    );
}

async fn a_failing_endpoint_is_retried_with_backoff_until_it_answers(
    Setup {
        app,
        state,
        session,
    }: Setup,
) {
    let (url, mut rx) = receiver(&[500, 503]).await;
    add_channel(&app, &session, &url).await;
    let error_app = create_error_app(&app, &session).await;

    let start = TimestampMillis::now().as_millis();
    send_error(&app, &error_app, "RuntimeException").await;
    assert_eq!(received(&mut rx).await["event"], "issue.new");
    let list = deliveries(&app, &session, 1).await;
    assert_eq!(list[0]["status"], "pending");
    assert_eq!(list[0]["attempts"], 1);
    assert_eq!(
        list[0]["error"],
        "webhook returned HTTP 500 Internal Server Error"
    );

    let at = |offset: i64| TimestampMillis::from_millis(start + offset);
    assert_eq!(retry_deliveries(&state, at(30_000)).await.unwrap(), 0);
    assert_eq!(retry_deliveries(&state, at(61_000)).await.unwrap(), 1);
    assert_eq!(received(&mut rx).await["event"], "issue.new");
    let list = deliveries(&app, &session, 1).await;
    assert_eq!(list[0]["status"], "pending");
    assert_eq!(list[0]["attempts"], 2);
    assert_eq!(
        list[0]["error"],
        "webhook returned HTTP 503 Service Unavailable"
    );

    // The second wait is five minutes.
    assert_eq!(
        retry_deliveries(&state, at(61_000 + 240_000))
            .await
            .unwrap(),
        0
    );
    let retry_at = 61_000 + 300_000 + 1_000;
    assert_eq!(retry_deliveries(&state, at(retry_at)).await.unwrap(), 1);
    assert_eq!(received(&mut rx).await["event"], "issue.new");
    let list = deliveries(&app, &session, 1).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["status"], "sent");
    assert_eq!(list[0]["attempts"], 3);
    assert_eq!(list[0]["next_attempt_at"], Value::Null);
    assert_eq!(list[0]["error"], Value::Null);

    assert_eq!(retry_deliveries(&state, at(86_400_000)).await.unwrap(), 0);
    assert!(rx.try_recv().is_err(), "no send after success");
}

async fn a_rejected_delivery_is_not_retried(
    Setup {
        app,
        state,
        session,
    }: Setup,
) {
    let (url, mut rx) = receiver(&[400]).await;
    add_channel(&app, &session, &url).await;
    let error_app = create_error_app(&app, &session).await;

    send_error(&app, &error_app, "RuntimeException").await;
    assert_eq!(received(&mut rx).await["event"], "issue.new");
    let list = deliveries(&app, &session, 1).await;
    assert_eq!(list[0]["status"], "failed");
    assert_eq!(list[0]["attempts"], 1);
    assert_eq!(list[0]["next_attempt_at"], Value::Null);
    assert_eq!(list[0]["error"], "webhook returned HTTP 400 Bad Request");

    let later = TimestampMillis::from_millis(TimestampMillis::now().as_millis() + 86_400_000);
    assert_eq!(retry_deliveries(&state, later).await.unwrap(), 0);
    assert!(rx.try_recv().is_err(), "no retry");
}
