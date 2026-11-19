//! Privacy guarantees: scrubbed error payloads, hashed user keys, data subject
//! requests, and the monthly analytics salt.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_platform::Config;
use owlee_server::{
    AppState, TestSession, boot, editor_session, member_session, router, testing_config,
};
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;

const BROWSER: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36";
/// `hash_user_key` under the development app key.
const ADA_KEY: &str = "957c08a814973a92f58c6fde7b473f56";
const IP_KEY: &str = "3175967edba216098b979bd5862a67fb";

fn config() -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    testing_config(path)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Value,
    session: Option<&TestSession>,
    extra: &[(&str, &str)],
) -> (StatusCode, Value) {
    let proxy = std::net::SocketAddr::from(([10, 0, 0, 2], 40000));
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .extension(axum::extract::ConnectInfo(proxy));
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-owlee-workspace", session.workspace_id.to_string());
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
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

fn failure(id: &str, user: Value) -> Value {
    json!({
        "event_id": id,
        "level": "error",
        "platform": "javascript",
        "exception": {"values": [{"type": "Error", "value": "card declined"}]},
        "user": user,
        "request": {
            "url": "https://shop.example/pay",
            "headers": [["Authorization", "Bearer live"], ["Accept", "text/html"]],
            "cookies": {"sid": "abc"},
            "data": {"password": "hunter2", "amount": 5}
        }
    })
}

async fn store_event(app: &Router, project: &str, key: &str, event: Value) {
    let (status, body) = send(
        app,
        "POST",
        &format!("/api/{project}/store/"),
        event,
        None,
        &[(
            "x-sentry-auth",
            &format!("Sentry sentry_key={key}, sentry_version=7"),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn create_app(app: &Router, session: &TestSession, name: &str) -> (String, String) {
    let (status, created) = send(
        app,
        "POST",
        "/api/error-apps",
        json!({"name": name, "platform": "javascript", "project_id": session.project_id}),
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

async fn stored(state: &AppState, id: &str) -> (String, Value) {
    let row = sqlx::query("SELECT user_key, payload FROM events WHERE id = $1")
        .bind(id)
        .fetch_one(state.db.pool())
        .await
        .unwrap();
    let payload: String = row.try_get("payload").unwrap();
    (
        row.try_get("user_key").unwrap(),
        serde_json::from_str(&payload).unwrap(),
    )
}

#[tokio::test]
async fn error_events_are_scrubbed_hashed_exported_and_erased() {
    let config = config();
    let state = boot(config.clone()).await.unwrap();
    let app = router(state.clone());
    let admin = editor_session(&state).await.unwrap();
    let member = member_session(&state, admin.workspace_id).await.unwrap();
    let (app_id, key) = create_app(&app, &admin, "Shop").await;
    let (project_id, key) = (app_id.as_str(), key.as_str());

    let ada = "a".repeat(32);
    let anonymous = "b".repeat(32);
    store_event(
        &app,
        project_id,
        key,
        failure(
            &ada,
            json!({"email": "ada@example.com", "ip_address": "203.0.113.9"}),
        ),
    )
    .await;
    store_event(
        &app,
        project_id,
        key,
        failure(&anonymous, json!({"ip_address": "203.0.113.9"})),
    )
    .await;

    let (user_key, payload) = stored(&state, &ada).await;
    assert_eq!(user_key, ADA_KEY);
    assert_eq!(payload["user"], json!({"email": "ada@example.com"}));
    assert_eq!(
        payload["request"],
        json!({
            "url": "https://shop.example/pay",
            "headers": [["Authorization", "[Filtered]"], ["Accept", "text/html"]],
            "data": {"password": "[Filtered]", "amount": 5}
        })
    );
    assert_eq!(stored(&state, &anonymous).await.0, IP_KEY);

    let export = "/api/privacy/errors?user=ada%40example.com";
    let (status, _) = send(&app, "GET", export, Value::Null, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&app, "GET", export, Value::Null, Some(&member), &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&app, "DELETE", export, Value::Null, Some(&member), &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(
        &app,
        "GET",
        "/api/privacy/errors?user=%20",
        Value::Null,
        Some(&admin),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let (status, body) = send(&app, "GET", export, Value::Null, Some(&admin), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["user"], "ada@example.com");
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{body}");
    assert_eq!(events[0]["id"], ada.as_str());
    assert_eq!(events[0]["appId"], project_id);
    assert_eq!(events[0]["appName"], "Shop");
    assert_eq!(events[0]["title"], "Error: card declined");
    assert_eq!(events[0]["payload"]["user"]["email"], "ada@example.com");

    let (status, body) = send(&app, "DELETE", export, Value::Null, Some(&admin), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"deleted": 1}));
    let (_, body) = send(&app, "GET", export, Value::Null, Some(&admin), &[]).await;
    assert_eq!(body["events"], json!([]));
    let (_, body) = send(
        &app,
        "GET",
        "/api/privacy/errors?user=203.0.113.9",
        Value::Null,
        Some(&admin),
        &[],
    )
    .await;
    assert_eq!(body["events"][0]["id"], anonymous.as_str());

    // The user key is the id, but the email still finds the event, in any case.
    let carol = "c".repeat(32);
    store_event(
        &app,
        project_id,
        key,
        failure(&carol, json!({"id": "7", "email": "carol@example.com"})),
    )
    .await;
    let by_email = "/api/privacy/errors?user=Carol%40Example.com";
    let (_, body) = send(&app, "GET", by_email, Value::Null, Some(&admin), &[]).await;
    assert_eq!(body["events"][0]["id"], carol.as_str(), "{body}");
    let (_, body) = send(&app, "DELETE", by_email, Value::Null, Some(&admin), &[]).await;
    assert_eq!(body, json!({"deleted": 1}));
    let (_, body) = send(
        &app,
        "GET",
        "/api/privacy/errors?user=7",
        Value::Null,
        Some(&admin),
        &[],
    )
    .await;
    assert_eq!(body["events"], json!([]));

    // A row from before the privacy migration: plain key cleared, raw payload.
    sqlx::query("UPDATE events SET user_key = NULL, payload = $1 WHERE id = $2")
        .bind(failure(&anonymous, json!({"ip_address": "203.0.113.9"})).to_string())
        .bind(&anonymous)
        .execute(state.db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM event_users WHERE event_id = $1")
        .bind(&anonymous)
        .execute(state.db.pool())
        .await
        .unwrap();
    let rebooted = boot(config).await.unwrap();
    let (user_key, payload) = stored(&rebooted, &anonymous).await;
    assert_eq!(user_key, IP_KEY);
    assert_eq!(payload["user"], json!({}));
    assert_eq!(payload["request"]["data"]["password"], "[Filtered]");
    assert_eq!(payload["request"].get("cookies"), None);
    let (_, body) = send(
        &router(rebooted.clone()),
        "GET",
        "/api/privacy/errors?user=203.0.113.9",
        Value::Null,
        Some(&admin),
        &[],
    )
    .await;
    assert_eq!(body["events"][0]["id"], anonymous.as_str(), "{body}");
}

#[tokio::test]
async fn analytics_salt_rotates_monthly_and_drops_older_salts() {
    let state = boot(config()).await.unwrap();
    let app = router(state.clone());
    let admin = editor_session(&state).await.unwrap();
    for (key, value) in [("secret", "legacy"), ("salt:2000-01", "old")] {
        sqlx::query("INSERT INTO insight_settings (key, value) VALUES ($1, $2)")
            .bind(key)
            .bind(value)
            .execute(state.db.pool())
            .await
            .unwrap();
    }
    let host = "docs.example";
    let (status, site) = send(
        &app,
        "POST",
        "/api/sites",
        json!({"name": "Docs", "domain": host, "project_id": admin.project_id}),
        Some(&admin),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{site}");
    let (status, body) = send(
        &app,
        "POST",
        "/api/receive",
        json!({"site": site["public_id"], "type": "pageview", "url": format!("https://{host}/")}),
        None,
        &[
            ("origin", &format!("https://{host}")),
            ("user-agent", BROWSER),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let month = chrono::Utc::now().format("salt:%Y-%m").to_string();
    let rows = sqlx::query("SELECT key, value FROM insight_settings ORDER BY key")
        .fetch_all(state.db.pool())
        .await
        .unwrap();
    let keys: Vec<String> = rows.iter().map(|row| row.try_get("key").unwrap()).collect();
    assert_eq!(keys, [month]);
    let salt: String = rows[0].try_get("value").unwrap();
    let session: String = sqlx::query("SELECT session_id FROM insight_events")
        .fetch_one(state.db.pool())
        .await
        .unwrap()
        .try_get("session_id")
        .unwrap();
    let timestamp: i64 = sqlx::query("SELECT timestamp FROM insight_events")
        .fetch_one(state.db.pool())
        .await
        .unwrap()
        .try_get("timestamp")
        .unwrap();
    let site_id = site["id"].as_str().unwrap();
    assert_eq!(
        session,
        owlee_insights::session_id(&salt, site_id, "10.0.0.2", BROWSER, timestamp)
    );
}

/// A request in one workspace finds and erases only that workspace's events,
/// even when the same person appears in another.
#[tokio::test]
async fn privacy_requests_stay_inside_the_workspace() {
    let state = boot(config()).await.unwrap();
    let app = router(state.clone());
    let first = editor_session(&state).await.unwrap();
    let second = editor_session(&state).await.unwrap();
    let (first_app, first_key) = create_app(&app, &first, "Shop").await;
    let (second_app, second_key) = create_app(&app, &second, "Blog").await;
    let in_first = "d".repeat(32);
    let in_second = "e".repeat(32);
    let ada = json!({"email": "ada@example.com"});
    store_event(
        &app,
        &first_app,
        &first_key,
        failure(&in_first, ada.clone()),
    )
    .await;
    store_event(&app, &second_app, &second_key, failure(&in_second, ada)).await;

    let export = "/api/privacy/errors?user=ada%40example.com";
    let (status, body) = send(&app, "GET", export, Value::Null, Some(&second), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [in_second.as_str()]);
    assert_eq!(body["events"][0]["appName"], "Blog");

    let (status, body) = send(&app, "DELETE", export, Value::Null, Some(&second), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"deleted": 1}));
    assert_eq!(stored(&state, &in_first).await.0, ADA_KEY);
    let (_, body) = send(&app, "GET", export, Value::Null, Some(&first), &[]).await;
    assert_eq!(body["events"][0]["id"], in_first.as_str(), "{body}");
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
}
