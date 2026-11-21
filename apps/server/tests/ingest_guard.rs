//! HTTP checks for the public ingest guards.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_platform::{Config, EnvironmentMode, TimestampMillis};
use owlee_server::{
    AppState, MapResolver, boot, editor_session, initialize_setup_token, prune_before, quota_today,
    recheck_verified, router, testing_config,
};
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;

const BROWSER: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36";

/// A signed-in user and the workspace and project their requests act in.
struct Session {
    cookie: String,
    workspace: String,
    project: String,
}

struct Hit {
    status: StatusCode,
    body: Value,
    retry_after: Option<String>,
    rate_limits: Option<String>,
    cookie: Option<String>,
}

async fn boot_app(mut config: Config) -> (Router, AppState, Option<String>) {
    if config.database_url.is_none() {
        let dir = tempfile::tempdir().unwrap();
        config.database_path = dir.path().join("owlee.sqlite");
        let _keep = Box::leak(Box::new(dir));
    }
    let state = boot(config).await.expect("boot");
    let token = initialize_setup_token(&state.db, TimestampMillis::now())
        .await
        .unwrap()
        .map(|issued| issued.token);
    let app = router(state.clone());
    (app, state, token)
}

fn site_host() -> String {
    let raw = owlee_platform::Id::new().to_string().replace('-', "");
    format!("d{}.example", &raw[..12])
}

fn sqlite_config() -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    testing_config(path)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    session: Option<&Session>,
    extra: &[(&str, &str)],
) -> Hit {
    // The peer is a proxy on the Docker network, as with Traefik in Coolify.
    let proxy = std::net::SocketAddr::from(([10, 0, 0, 2], 40000));
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .extension(axum::extract::ConnectInfo(proxy));
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-owlee-workspace", &session.workspace);
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let retry_after = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let rate_limits = headers
        .get("x-sentry-rate-limits")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let cookie = headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| value.split(';').next())
        .filter(|value| value.starts_with("owlee_session="))
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Hit {
        status,
        body,
        retry_after,
        rate_limits,
        cookie,
    }
}

/// The root user when setup is still open, so instance settings are reachable.
/// On a reused Postgres database, a fresh editor instead, with the same password,
/// so the login tests work on both paths.
async fn owner(app: &Router, state: &AppState, setup_token: Option<&str>) -> Session {
    let Some(token) = setup_token else {
        let session = editor_session(state).await.unwrap();
        let hash = state.passwords.hash("correct-horse-battery").unwrap();
        sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
            .bind(hash)
            .bind(session.user_id.to_string())
            .execute(state.db.pool())
            .await
            .unwrap();
        return Session {
            cookie: session.cookie,
            workspace: session.workspace_id.to_string(),
            project: session.project_id.to_string(),
        };
    };
    let body = json!({
        "token": token,
        "email": format!("guard-{}@coollabs.io", owlee_platform::Id::new()),
        "password": "correct-horse-battery",
        "display_name": "Ada"
    });
    let hit = send(
        app,
        "POST",
        "/api/setup/complete",
        body.to_string().into_bytes(),
        None,
        &[("content-type", "application/json")],
    )
    .await;
    assert!(hit.status.is_success(), "{} {}", hit.status, hit.body);
    let mut session = Session {
        cookie: hit.cookie.expect("session"),
        workspace: String::new(),
        project: String::new(),
    };
    let me = send(app, "GET", "/api/auth/me", Vec::new(), Some(&session), &[]).await;
    session.workspace = me.body["workspaces"][0]["id"]
        .as_str()
        .expect("workspace")
        .to_owned();
    let projects = send(app, "GET", "/api/projects", Vec::new(), Some(&session), &[]).await;
    session.project = projects.body[0]["id"].as_str().expect("project").to_owned();
    session
}

fn envelope(event_json: &str) -> Vec<u8> {
    let header = format!(r#"{{"type":"event","length":{}}}"#, event_json.len());
    let mut body = b"{}\n".to_vec();
    body.extend_from_slice(header.as_bytes());
    body.push(b'\n');
    body.extend_from_slice(event_json.as_bytes());
    body
}

fn event_id() -> String {
    owlee_platform::Id::new().to_string().replace('-', "")
}

fn event(id: &str, message: &str) -> Value {
    json!({
        "event_id": id,
        "level": "error",
        "platform": "javascript",
        "exception": {
            "values": [{
                "type": "Error",
                "value": message,
                "stacktrace": {"frames": [{"filename": "app.js", "function": "main", "in_app": true}]}
            }]
        }
    })
}

async fn error_app(app: &Router, cookie: &Session) -> (String, String) {
    let hit = send(
        app,
        "POST",
        "/api/error-apps",
        json!({"name": "Checkout", "platform": "javascript", "project_id": cookie.project})
            .to_string()
            .into_bytes(),
        Some(cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(hit.status, StatusCode::OK, "{}", hit.body);
    (
        hit.body["id"].as_str().unwrap().to_owned(),
        hit.body["publicKey"].as_str().unwrap().to_owned(),
    )
}

async fn post_event(app: &Router, id: &str, key: &str, body: Vec<u8>) -> Hit {
    send(
        app,
        "POST",
        &format!("/api/{id}/envelope/?sentry_key={key}"),
        body,
        None,
        &[
            ("content-type", "application/x-sentry-envelope"),
            (
                "x-sentry-auth",
                &format!("Sentry sentry_key={key}, sentry_version=7"),
            ),
        ],
    )
    .await
}

async fn exercise(config: Config) {
    oversized_body(&config).await;
    burst_is_429(config.clone()).await;
    inflating_body_is_413(config.clone()).await;
    wrong_keys_spend_the_ip_limit(config.clone()).await;
    envelope_stores_one_event(config.clone()).await;
    event_ids_do_not_collide_across_apps(config.clone()).await;
    retention_drops_issues_without_events(config.clone()).await;
    wrong_origin(config.clone()).await;
    flood_is_capped(config.clone()).await;
    retention_keeps_rollups(config.clone()).await;
    quota_is_enforced(config.clone()).await;
    override_replaces_the_global_cap(config.clone()).await;
    txt_verification(config.clone()).await;
    login_is_limited(config.clone()).await;
    wrong_guesses_do_not_block_the_right_password(config).await;
}

async fn oversized_body(config: &Config) {
    let (app, state, token) = boot_app(config.clone()).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    let mut huge = "x".repeat(config.ingest.error_max_body_bytes + 8);
    huge.insert_str(0, "{");
    let hit = post_event(&app, &id, &key, huge.into_bytes()).await;
    assert_eq!(hit.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", hit.body);

    let (app, state, token) = boot_app(config.clone()).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let created = send(
        &app,
        "POST",
        "/api/sites",
        json!({"name": "Docs", "domain": site_host(), "project_id": cookie.project})
            .to_string()
            .into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let public_id = created.body["public_id"].as_str().unwrap();
    let fat = "y".repeat(config.ingest.analytics_max_body_bytes + 8);
    let body = format!(
        r#"{{"site":"{public_id}","type":"pageview","url":"https://docs.example/","pad":"{fat}"}}"#
    );
    let rejected = send(
        &app,
        "POST",
        "/api/receive",
        body.into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            ("origin", "https://docs.example"),
            ("user-agent", BROWSER),
        ],
    )
    .await;
    assert_eq!(
        rejected.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        rejected.body
    );

    // A chunked body has no Content-Length. The read must stop at the limit,
    // so an endless stream is answered 413 instead of being buffered forever.
    let endless = futures_util::stream::unfold((), |()| async {
        tokio::task::yield_now().await;
        let chunk = axum::body::Bytes::from_static(&[b' '; 1024]);
        Some((Ok::<_, std::convert::Infallible>(chunk), ()))
    });
    let request = Request::builder()
        .method("POST")
        .uri("/api/receive")
        .header("content-type", "application/json")
        .header("origin", "https://docs.example")
        .header("user-agent", BROWSER)
        .body(Body::from_stream(endless))
        .unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        app.clone().oneshot(request),
    )
    .await
    .expect("endless analytics body must be cut off at the limit")
    .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

async fn burst_is_429(mut config: Config) {
    config.ingest.error_rate_ip_burst = 2;
    config.ingest.error_rate_ip_per_min = 2;
    config.ingest.error_rate_project_burst = 100;
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    for index in 0..2 {
        let body = envelope(&event(&event_id(), "disk full").to_string());
        let hit = post_event(&app, &id, &key, body).await;
        assert_eq!(hit.status, StatusCode::OK, "{}", hit.body);
    }
    let blocked = post_event(
        &app,
        &id,
        &key,
        envelope(&event(&event_id(), "disk full").to_string()),
    )
    .await;
    assert_eq!(
        blocked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        blocked.body
    );
    let retry = blocked.retry_after.as_deref().unwrap_or("");
    assert!(!retry.is_empty() && retry != "0", "{retry}");
    assert!(
        blocked
            .rate_limits
            .as_deref()
            .unwrap_or("")
            .contains(":error:"),
        "{:?}",
        blocked.rate_limits
    );
    let listed = send(
        &app,
        "GET",
        &format!("/api/error-apps/{id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    let intake = &listed.body["intake"];
    assert_eq!(intake["stored"], 2, "{intake}");
    assert_eq!(intake["dropped"], 1, "{intake}");
    assert_eq!(
        intake["last_drop"]["reason"], "client_rate_limit",
        "{intake}"
    );
}

async fn count(state: &AppState, sql: &str, id: &str) -> i64 {
    sqlx::query(sql)
        .bind(id)
        .fetch_one(state.db.pool())
        .await
        .unwrap()
        .try_get::<i64, _>("n")
        .unwrap()
}

async fn intake_of(app: &Router, id: &str, cookie: &Session) -> Value {
    let listed = send(
        app,
        "GET",
        &format!("/api/error-apps/{id}"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    listed.body["intake"].clone()
}

async fn inflating_body_is_413(config: Config) {
    use std::io::Write;
    let (app, state, token) = boot_app(config.clone()).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    let mut event = event(&event_id(), "bomb");
    event["extra"] = json!({"pad": "a".repeat(config.ingest.error_max_body_bytes * 2)});
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gzip.write_all(&envelope(&event.to_string())).unwrap();
    let small = gzip.finish().unwrap();
    assert!(small.len() < 16 * 1024, "{}", small.len());
    for uri in [
        format!("/api/{id}/envelope/?sentry_key={key}"),
        format!("/api/{id}/store/?sentry_key={key}"),
    ] {
        let hit = send(
            &app,
            "POST",
            &uri,
            small.clone(),
            None,
            &[("content-encoding", "gzip")],
        )
        .await;
        assert_eq!(
            hit.status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{uri}: {}",
            hit.body
        );
    }
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) AS n FROM events WHERE app_id = $1",
            &id
        )
        .await,
        0
    );
}

async fn wrong_keys_spend_the_ip_limit(mut config: Config) {
    config.ingest.error_rate_ip_burst = 2;
    config.ingest.error_rate_ip_per_min = 2;
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    let body = || envelope(&event(&event_id(), "guess").to_string());
    let wrong = post_event(&app, &id, "not-the-key", body()).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED, "{}", wrong.body);
    let junk = send(
        &app,
        "POST",
        &format!("/api/{id}/store/?sentry_key={key}"),
        b"not json".to_vec(),
        None,
        &[],
    )
    .await;
    assert_eq!(junk.status, StatusCode::BAD_REQUEST, "{}", junk.body);
    for uri in [
        format!("/api/{id}/envelope/?sentry_key=not-the-key"),
        format!("/api/{id}/store/?sentry_key=not-the-key"),
        format!("/api/{id}/envelope/?sentry_key={key}"),
    ] {
        let blocked = send(&app, "POST", &uri, body(), None, &[]).await;
        assert_eq!(
            blocked.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{uri}: {}",
            blocked.body
        );
    }
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) AS n FROM events WHERE app_id = $1",
            &id
        )
        .await,
        0
    );
    // Nothing was accepted for this app, so the refusals made before the key
    // check are not counted against it.
    assert_eq!(intake_of(&app, &id, &cookie).await["dropped"], 0);
}

async fn envelope_stores_one_event(config: Config) {
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    let first = event_id();
    let mut body = b"{}\n".to_vec();
    for (event_id, message) in [
        (first.clone(), "first failure"),
        (event_id(), "second failure"),
        (event_id(), "third failure"),
    ] {
        let payload = event(&event_id, message).to_string();
        body.extend_from_slice(
            format!("{{\"type\":\"event\",\"length\":{}}}\n", payload.len()).as_bytes(),
        );
        body.extend_from_slice(payload.as_bytes());
        body.push(b'\n');
    }
    let hit = post_event(&app, &id, &key, body).await;
    assert_eq!(hit.status, StatusCode::OK, "{}", hit.body);
    assert_eq!(hit.body["id"], first);
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) AS n FROM events WHERE app_id = $1",
            &id
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) AS n FROM issues WHERE app_id = $1",
            &id
        )
        .await,
        1
    );
    let intake = intake_of(&app, &id, &cookie).await;
    assert_eq!(intake["stored"], 1, "{intake}");
    assert_eq!(intake["dropped"], 2, "{intake}");
    assert_eq!(intake["last_drop"]["reason"], "malformed", "{intake}");
}

async fn event_ids_do_not_collide_across_apps(config: Config) {
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (first_app, first_key) = error_app(&app, &cookie).await;
    let (second_app, second_key) = error_app(&app, &cookie).await;
    let shared = event_id();
    let body = || envelope(&event(&shared, "same id").to_string());

    let original = post_event(&app, &first_app, &first_key, body()).await;
    assert_eq!(original.status, StatusCode::OK, "{}", original.body);
    assert_eq!(original.body["id"], shared);
    let resent = post_event(&app, &first_app, &first_key, body()).await;
    assert_eq!(resent.body["id"], shared);

    let other = post_event(&app, &second_app, &second_key, body()).await;
    assert_eq!(other.status, StatusCode::OK, "{}", other.body);
    let other_id = other.body["id"].as_str().unwrap();
    assert_ne!(other_id, shared);
    assert_eq!(other_id.len(), 32);
    let other_resent = post_event(&app, &second_app, &second_key, body()).await;
    assert_eq!(other_resent.body["id"], other_id, "a resend keeps its id");

    let events = "SELECT COUNT(*) AS n FROM events WHERE app_id = $1";
    assert_eq!(count(&state, events, &first_app).await, 1);
    assert_eq!(count(&state, events, &second_app).await, 1);
    let owner_of_shared = sqlx::query("SELECT app_id FROM events WHERE id = $1")
        .bind(&shared)
        .fetch_one(state.db.pool())
        .await
        .unwrap()
        .try_get::<String, _>("app_id")
        .unwrap();
    assert_eq!(owner_of_shared, first_app);
}

async fn retention_drops_issues_without_events(config: Config) {
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    for message in ["expired failure", "erased failure", "live failure"] {
        let mut event = event(&event_id(), message);
        event["fingerprint"] = json!([message]);
        let hit = post_event(&app, &id, &key, envelope(&event.to_string())).await;
        assert_eq!(hit.status, StatusCode::OK, "{}", hit.body);
    }
    // Aged out: every event and the last sighting are past retention.
    for sql in [
        "UPDATE events SET timestamp = '2000-01-01T00:00:00.000Z' WHERE app_id = $1 AND title = 'Error: expired failure'",
        "UPDATE issues SET last_seen = '2000-01-01T00:00:00.000Z' WHERE app_id = $1 AND title = 'Error: expired failure'",
        // Seen recently, events removed some other way: stays.
        "DELETE FROM events WHERE app_id = $1 AND title = 'Error: erased failure'",
    ] {
        let changed = sqlx::query(sql)
            .bind(&id)
            .execute(state.db.pool())
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(changed, 1, "{sql}");
    }
    let stats = prune_before(&state.db, "2020-01-01T00:00:00.000Z", 0)
        .await
        .unwrap();
    assert!(stats.error_events >= 1);
    assert!(stats.error_issues >= 1);
    let rows = sqlx::query("SELECT title FROM issues WHERE app_id = $1 ORDER BY title")
        .bind(&id)
        .fetch_all(state.db.pool())
        .await
        .unwrap();
    let titles: Vec<String> = rows
        .iter()
        .map(|row| row.try_get("title").unwrap())
        .collect();
    assert_eq!(titles, ["Error: erased failure", "Error: live failure"]);
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) AS n FROM event_users WHERE event_id IN (SELECT id FROM events WHERE app_id = $1 AND timestamp < '2020')",
            &id
        )
        .await,
        0
    );
}

async fn wrong_origin(config: Config) {
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let created = send(
        &app,
        "POST",
        "/api/sites",
        json!({"name": "Docs", "domain": site_host(), "project_id": cookie.project})
            .to_string()
            .into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    let public_id = created.body["public_id"].as_str().unwrap().to_owned();
    let site_id = created.body["id"].as_str().unwrap().to_owned();
    let domain = created.body["domain"].as_str().unwrap().to_owned();
    let payload = json!({
        "site": public_id,
        "type": "pageview",
        "url": "https://evil.example/"
    })
    .to_string()
    .into_bytes();
    let rejected = send(
        &app,
        "POST",
        "/api/receive",
        payload.clone(),
        None,
        &[
            ("content-type", "application/json"),
            ("origin", "https://evil.example"),
            ("user-agent", BROWSER),
        ],
    )
    .await;
    assert_eq!(rejected.status, StatusCode::FORBIDDEN, "{}", rejected.body);
    let accepted = send(
        &app,
        "POST",
        "/api/receive",
        json!({"site": public_id, "type": "pageview", "url": format!("https://{domain}/guide")})
            .to_string()
            .into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            ("origin", &format!("https://{domain}")),
            ("user-agent", BROWSER),
        ],
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{}", accepted.body);
    let rows = sqlx::query("SELECT COUNT(*) AS n FROM insight_events WHERE site_id = $1")
        .bind(&site_id)
        .fetch_one(state.db.pool())
        .await
        .unwrap();
    let count: i64 = rows.try_get("n").unwrap();
    assert_eq!(count, 1, "the rejected origin was not stored");
    let site = send(
        &app,
        "GET",
        &format!("/api/sites/{site_id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    let intake = &site.body["intake"];
    assert_eq!(intake["stored"], 1, "{intake}");
    assert_eq!(intake["dropped"], 1, "{intake}");
    assert_eq!(
        intake["last_drop"]["reason"], "origin_not_allowed",
        "{intake}"
    );
}

async fn flood_is_capped(config: Config) {
    let keep = config.ingest.error_keep_first;
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    for _ in 0..8 {
        let body = envelope(&event(&event_id(), "disk full on volume 3").to_string());
        let hit = post_event(&app, &id, &key, body).await;
        assert_eq!(hit.status, StatusCode::OK, "{}", hit.body);
    }
    let feed = send(
        &app,
        "GET",
        &format!("/api/error-issues?app_id={id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(feed.status, StatusCode::OK, "{}", feed.body);
    assert_eq!(feed.body["issues"].as_array().unwrap().len(), 1);
    assert_eq!(feed.body["issues"][0]["eventCount"], 8);
    let issue_id = feed.body["issues"][0]["id"].as_str().unwrap();
    let detail = send(
        &app,
        "GET",
        &format!("/api/error-issues/{issue_id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    let stored = detail.body["events"].as_array().unwrap().len() as i64;
    assert!(stored <= keep, "stored {stored} payloads, cap is {keep}");
    assert!(stored >= 1);
}

async fn retention_keeps_rollups(config: Config) {
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    let first = post_event(
        &app,
        &id,
        &key,
        envelope(&event(&event_id(), "old failure").to_string()),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let second = post_event(
        &app,
        &id,
        &key,
        envelope(&event(&event_id(), "old failure").to_string()),
    )
    .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    let created = send(
        &app,
        "POST",
        "/api/sites",
        json!({"name": "Docs", "domain": site_host(), "project_id": cookie.project})
            .to_string()
            .into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    let public_id = created.body["public_id"].as_str().unwrap();
    let site_id = created.body["id"].as_str().unwrap().to_owned();
    let domain = created.body["domain"].as_str().unwrap().to_owned();
    let viewed = send(
        &app,
        "POST",
        "/api/receive",
        json!({"site": public_id, "type": "pageview", "url": format!("https://{domain}/")})
            .to_string()
            .into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            ("origin", &format!("https://{domain}")),
            ("user-agent", BROWSER),
        ],
    )
    .await;
    assert_eq!(viewed.status, StatusCode::ACCEPTED, "{}", viewed.body);
    let signup = send(
        &app,
        "POST",
        "/api/receive",
        json!({"site": public_id, "type": "signup", "url": format!("https://{domain}/"), "data": {"plan": "pro"}})
            .to_string()
            .into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            ("origin", &format!("https://{domain}")),
            ("user-agent", BROWSER),
        ],
    )
    .await;
    assert_eq!(signup.status, StatusCode::ACCEPTED, "{}", signup.body);
    let raw_rows = [
        "SELECT COUNT(*) AS n FROM insight_events WHERE site_id = $1",
        "SELECT COUNT(*) AS n FROM insight_event_data WHERE site_id = $1",
        "SELECT COUNT(*) AS n FROM insight_visits WHERE site_id = $1",
        "SELECT COUNT(*) AS n FROM insight_site_sessions WHERE site_id = $1",
    ];
    let mut counts = Vec::new();
    for sql in raw_rows {
        let row = sqlx::query(sql)
            .bind(&site_id)
            .fetch_one(state.db.pool())
            .await
            .unwrap();
        counts.push(row.try_get::<i64, _>("n").unwrap());
    }
    assert_eq!(counts, [2, 1, 1, 1]);
    let old_id = first.body["id"].as_str().unwrap();
    sqlx::query("UPDATE events SET timestamp = '2000-01-01T00:00:00.000Z' WHERE id = $1")
        .bind(old_id)
        .execute(state.db.pool())
        .await
        .unwrap();
    for sql in [
        "UPDATE insight_events SET timestamp = 10 WHERE site_id = $1",
        "UPDATE insight_visits SET started_at = 10, last_at = 10 WHERE site_id = $1",
        "UPDATE insight_site_sessions SET day = 0 WHERE site_id = $1",
    ] {
        sqlx::query(sql)
            .bind(&site_id)
            .execute(state.db.pool())
            .await
            .unwrap();
    }
    let stats = prune_before(&state.db, "2020-01-01T00:00:00.000Z", 1_000_000)
        .await
        .unwrap();
    assert!(stats.error_events >= 1);
    assert!(stats.analytics_events >= 1);
    let feed = send(
        &app,
        "GET",
        &format!("/api/error-issues?app_id={id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(feed.body["issues"][0]["eventCount"], 2);
    let issue_id = feed.body["issues"][0]["id"].as_str().unwrap();
    let detail = send(
        &app,
        "GET",
        &format!("/api/error-issues/{issue_id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(detail.body["events"].as_array().unwrap().len(), 1);
    let overview = send(
        &app,
        "GET",
        &format!("/api/sites/{site_id}/overview"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.body);
    assert_eq!(overview.body["pageviews"], 1);
    for sql in raw_rows {
        let left = sqlx::query(sql)
            .bind(&site_id)
            .fetch_one(state.db.pool())
            .await
            .unwrap();
        assert_eq!(left.try_get::<i64, _>("n").unwrap(), 0, "{sql}");
    }
}

async fn quota_is_enforced(config: Config) {
    let (app, state, token) = boot_app(config).await;
    let root = token.is_some();
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    if root {
        let before = send(
            &app,
            "GET",
            "/api/admin/settings",
            Vec::new(),
            Some(&cookie),
            &[],
        )
        .await;
        assert_eq!(before.status, StatusCode::OK, "{}", before.body);
        let error_quota = before.body["error_daily_quota"].as_i64().unwrap();
        let analytics_quota = before.body["analytics_daily_quota"].as_i64().unwrap();
        let updated = send(
            &app,
            "PUT",
            "/api/admin/settings/quotas",
            json!({"error_daily": error_quota, "analytics_daily": analytics_quota + 1})
                .to_string()
                .into_bytes(),
            Some(&cookie),
            &[("content-type", "application/json")],
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
        assert_eq!(updated.body["analytics_daily_quota"], analytics_quota + 1);
        let restored = send(
            &app,
            "PUT",
            "/api/admin/settings/quotas",
            json!({"error_daily": error_quota, "analytics_daily": analytics_quota})
                .to_string()
                .into_bytes(),
            Some(&cookie),
            &[("content-type", "application/json")],
        )
        .await;
        assert_eq!(restored.status, StatusCode::OK, "{}", restored.body);
    }
    let current = send(
        &app,
        "GET",
        &format!("/api/error-apps/{id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let error_quota = current.body["dailyQuota"].as_i64().unwrap();
    sqlx::query(
        "INSERT INTO ingest_usage (kind, subject_id, day, count) VALUES ('error', $1, $2, $3) \
         ON CONFLICT (kind, subject_id, day) DO UPDATE SET count = $3",
    )
    .bind(&id)
    .bind(quota_today())
    .bind(error_quota)
    .execute(state.db.pool())
    .await
    .unwrap();
    let blocked = post_event(
        &app,
        &id,
        &key,
        envelope(&event(&event_id(), "over quota").to_string()),
    )
    .await;
    assert_eq!(
        blocked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        blocked.body
    );
    assert!(blocked.retry_after.is_some());
    assert!(
        blocked
            .rate_limits
            .as_deref()
            .unwrap_or("")
            .contains("quota_exceeded"),
        "{:?}",
        blocked.rate_limits
    );
    let listed = send(
        &app,
        "GET",
        "/api/error-apps",
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    let listed_app = listed
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap();
    assert_eq!(listed_app["quotaExceeded"], true);
    sqlx::query("DELETE FROM ingest_usage WHERE subject_id = $1")
        .bind(&id)
        .execute(state.db.pool())
        .await
        .unwrap();
}

async fn override_replaces_the_global_cap(config: Config) {
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let (id, key) = error_app(&app, &cookie).await;
    let first = send(
        &app,
        "GET",
        &format!("/api/error-apps/{id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert!(first.body["quotaOverride"].is_null());
    let inherited = first.body["dailyQuota"].as_i64().unwrap();
    assert!(
        inherited > 1,
        "global cap {inherited} should be above the override"
    );
    assert!(!first.body["resetsAt"].as_str().unwrap_or("").is_empty());

    let set = send(
        &app,
        "PUT",
        &format!("/api/error-apps/{id}/quota"),
        json!({"daily_quota": 1}).to_string().into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    assert_eq!(set.body["dailyQuota"], 1);
    assert_eq!(set.body["quotaOverride"], 1);

    let accepted = post_event(
        &app,
        &id,
        &key,
        envelope(&event(&event_id(), "over the app cap").to_string()),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    let blocked = post_event(
        &app,
        &id,
        &key,
        envelope(&event(&event_id(), "over the app cap").to_string()),
    )
    .await;
    assert_eq!(
        blocked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        blocked.body
    );
    assert!(
        blocked
            .rate_limits
            .as_deref()
            .unwrap_or("")
            .contains("quota_exceeded"),
        "{:?}",
        blocked.rate_limits
    );

    let home = send(
        &app,
        "GET",
        "/api/dashboard",
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(home.status, StatusCode::OK, "{}", home.body);
    let hits = home.body["quota_hits"].as_array().unwrap();
    assert!(
        hits.iter()
            .any(|hit| hit["kind"] == "error" && hit["id"] == id),
        "{hits:?}"
    );
    assert!(!hits[0]["resets_at"].as_str().unwrap_or("").is_empty());

    let cleared = send(
        &app,
        "PUT",
        &format!("/api/error-apps/{id}/quota"),
        json!({"daily_quota": null}).to_string().into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    assert!(cleared.body["quotaOverride"].is_null());
    assert_eq!(cleared.body["dailyQuota"], inherited);

    let domain = site_host();
    let created = send(
        &app,
        "POST",
        "/api/sites",
        json!({"name": "Docs", "domain": domain.clone(), "project_id": cookie.project})
            .to_string()
            .into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert!(created.body["quota_override"].is_null());
    let site_inherited = created.body["daily_quota"].as_i64().unwrap();
    assert!(site_inherited > 1);
    let site_id = created.body["id"].as_str().unwrap().to_owned();
    let public_id = created.body["public_id"].as_str().unwrap().to_owned();
    let set_site = send(
        &app,
        "PUT",
        &format!("/api/sites/{site_id}/quota"),
        json!({"daily_quota": 1}).to_string().into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(set_site.status, StatusCode::OK, "{}", set_site.body);
    assert_eq!(set_site.body["daily_quota"], 1);
    assert_eq!(set_site.body["quota_override"], 1);
    let origin = format!("https://{domain}");
    let first_view = receive(&app, &public_id, &origin).await;
    assert_eq!(
        first_view.status,
        StatusCode::ACCEPTED,
        "{}",
        first_view.body
    );
    let second_view = receive(&app, &public_id, &origin).await;
    assert_eq!(
        second_view.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        second_view.body
    );
    assert!(second_view.retry_after.is_some());
    let again = send(
        &app,
        "GET",
        "/api/dashboard",
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    let site_hits = again.body["quota_hits"].as_array().unwrap();
    assert!(
        site_hits
            .iter()
            .any(|hit| hit["kind"] == "analytics" && hit["id"] == site_id),
        "{site_hits:?}"
    );
}

async fn txt_verification(mut config: Config) {
    config.environment = EnvironmentMode::Production;
    config.ingest.allow_localhost = false;
    let mut state = boot(config).await.unwrap();
    let dns = MapResolver::default();
    state.dns = Arc::new(dns.clone());
    let token = initialize_setup_token(&state.db, TimestampMillis::now())
        .await
        .unwrap()
        .map(|issued| issued.token);
    let app = router(state.clone());
    let cookie = owner(&app, &state, token.as_deref()).await;
    let domain = site_host();
    let created = send(
        &app,
        "POST",
        "/api/sites",
        json!({"name": "Example", "domain": domain.clone(), "project_id": cookie.project})
            .to_string()
            .into_bytes(),
        Some(&cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["verified"], false);
    assert_eq!(created.body["ingest_open"], false);
    let site_id = created.body["id"].as_str().unwrap();
    let public_id = created.body["public_id"].as_str().unwrap();
    let name = created.body["verify_name"].as_str().unwrap().to_owned();
    let value = created.body["verify_value"].as_str().unwrap().to_owned();
    let origin = format!("https://{domain}");
    assert_eq!(name, format!("_owlee-verify.{domain}"));
    assert!(value.starts_with("owlee-verify="));
    let early = receive(&app, public_id, &origin).await;
    assert_eq!(early.status, StatusCode::FORBIDDEN, "{}", early.body);

    dns.set(&name, vec!["unrelated".into()]);
    let failed = send(
        &app,
        "POST",
        &format!("/api/sites/{site_id}/verify"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(failed.status, StatusCode::BAD_REQUEST, "{}", failed.body);
    let still = get_site(&app, site_id, &cookie).await;
    assert_eq!(still["verified"], false);

    dns.set(&name, vec![value.clone()]);
    let ok = send(
        &app,
        "POST",
        &format!("/api/sites/{site_id}/verify"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["verified"], true);
    let accepted = receive(&app, public_id, &origin).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{}", accepted.body);

    dns.set(&name, Vec::new());
    let cleared = recheck_verified(&state).await.unwrap();
    assert_eq!(cleared, 1);
    let after = get_site(&app, site_id, &cookie).await;
    assert_eq!(after["verified"], false);
    let again = receive(&app, public_id, &origin).await;
    assert_eq!(again.status, StatusCode::FORBIDDEN, "{}", again.body);
}

async fn receive(app: &Router, public_id: &str, origin: &str) -> Hit {
    send(
        app,
        "POST",
        "/api/receive",
        json!({"site": public_id, "type": "pageview", "url": format!("{origin}/")})
            .to_string()
            .into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            ("origin", origin),
            ("user-agent", BROWSER),
        ],
    )
    .await
}

async fn get_site(app: &Router, id: &str, cookie: &Session) -> Value {
    let hit = send(
        app,
        "GET",
        &format!("/api/sites/{id}"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    assert_eq!(hit.status, StatusCode::OK, "{}", hit.body);
    hit.body
}

async fn login_is_limited(mut config: Config) {
    config.proxy_hops = 1;
    config.ingest.login_rate_ip_burst = 2;
    config.ingest.login_rate_ip_per_min = 2;
    config.ingest.login_rate_account_burst = 100;
    config.ingest.login_rate_account_per_min = 100;
    let (app, state, token) = boot_app(config).await;
    let _cookie = owner(&app, &state, token.as_deref()).await;
    let body = json!({"email": "nobody@coollabs.io", "password": "wrong-password-1"})
        .to_string()
        .into_bytes();
    for index in 0..2 {
        let attempt = send(
            &app,
            "POST",
            "/api/auth/login",
            body.clone(),
            None,
            &[
                ("content-type", "application/json"),
                ("x-forwarded-for", "203.0.113.10"),
            ],
        )
        .await;
        assert_eq!(attempt.status, StatusCode::UNAUTHORIZED, "{index}");
    }
    let blocked = send(
        &app,
        "POST",
        "/api/auth/login",
        body.clone(),
        None,
        &[
            ("content-type", "application/json"),
            ("x-forwarded-for", "203.0.113.10"),
        ],
    )
    .await;
    assert_eq!(
        blocked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        blocked.body
    );
    assert!(blocked.retry_after.is_some());
    let other_ip = send(
        &app,
        "POST",
        "/api/auth/login",
        body,
        None,
        &[
            ("content-type", "application/json"),
            ("x-forwarded-for", "203.0.113.11"),
        ],
    )
    .await;
    assert_eq!(
        other_ip.status,
        StatusCode::UNAUTHORIZED,
        "a different forwarded address has its own bucket"
    );
}

async fn login(app: &Router, email: &str, password: &str, ip: &str) -> Hit {
    send(
        app,
        "POST",
        "/api/auth/login",
        json!({"email": email, "password": password})
            .to_string()
            .into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            ("x-forwarded-for", ip),
        ],
    )
    .await
}

async fn wrong_guesses_do_not_block_the_right_password(mut config: Config) {
    config.proxy_hops = 1;
    config.ingest.login_rate_account_burst = 2;
    config.ingest.login_rate_account_per_min = 1;
    let (app, state, token) = boot_app(config).await;
    let cookie = owner(&app, &state, token.as_deref()).await;
    let me = send(&app, "GET", "/api/auth/me", Vec::new(), Some(&cookie), &[]).await;
    let email = me.body["email"].as_str().expect("me.email").to_owned();

    // A correct login does not use the account bucket.
    for _ in 0..3 {
        let ok = login(&app, &email, "correct-horse-battery", "203.0.113.20").await;
        assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    }
    // A random wrong guess, so the test has no fixed credential in it.
    let guess = format!("guess-{}", owlee_platform::Id::new());
    // Wrong guesses use up the bucket of the address that sends them.
    for _ in 0..2 {
        let bad = login(&app, &email, &guess, "203.0.113.21").await;
        assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);
    }
    let blocked = login(&app, &email, &guess, "203.0.113.21").await;
    assert_eq!(
        blocked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        blocked.body
    );

    // An unknown email is limited the same way, so the answer does not differ.
    for _ in 0..2 {
        let bad = login(&app, "ghost@coollabs.io", &guess, "203.0.113.23").await;
        assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);
    }
    let ghost = login(&app, "ghost@coollabs.io", &guess, "203.0.113.23").await;
    assert_eq!(
        ghost.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        ghost.body
    );
}

#[tokio::test]
async fn sqlite_ingest_guards() {
    exercise(sqlite_config()).await;
}

#[tokio::test]
async fn postgres_ingest_guards() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| url.starts_with("postgres"))
    else {
        return;
    };
    let mut config = sqlite_config();
    // Own database so this test's owner is root and it does not share
    // instance_settings with the other Postgres suites.
    let url = url
        .trim_end_matches('/')
        .rsplit_once('/')
        .map_or(url.clone(), |(base, _)| format!("{base}/owlee_ingest"));
    let admin = url.replacen("owlee_ingest", "postgres", 1);
    let _ = sqlx::query("CREATE DATABASE owlee_ingest")
        .execute(
            &sqlx::postgres::PgPoolOptions::new()
                .connect(&admin)
                .await
                .unwrap(),
        )
        .await;
    config.database_url = Some(url);
    exercise(config).await;
}
