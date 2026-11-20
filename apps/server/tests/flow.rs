use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_platform::TimestampMillis;
use owlee_server::{
    AppState, DEMO_PASSWORD, TestSession, boot, editor_session, initialize_setup_token,
    member_session, router, testing_config,
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A signed-in user acting inside one workspace.
struct Who {
    cookie: String,
    workspace: String,
    project: String,
}

impl From<TestSession> for Who {
    fn from(session: TestSession) -> Self {
        Self {
            cookie: session.cookie,
            workspace: session.workspace_id.to_string(),
            project: session.project_id.to_string(),
        }
    }
}

async fn app() -> (Router, Option<String>) {
    let (router, _, token) = app_with_state(Vec::new()).await;
    (router, token)
}

async fn app_with_targets(connectivity_targets: Vec<String>) -> (Router, Option<String>) {
    let (router, _, token) = app_with_state(connectivity_targets).await;
    (router, token)
}

async fn app_with_state(connectivity_targets: Vec<String>) -> (Router, AppState, Option<String>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    // Leak the temp dir so the sqlite file outlives the router.
    let _keep = Box::leak(Box::new(dir));
    let mut config = testing_config(path);
    config.connectivity_targets = connectivity_targets;
    let state = boot(config).await.unwrap();
    let token = initialize_setup_token(&state.db, TimestampMillis::now())
        .await
        .unwrap()
        .map(|issued| issued.token);
    (router(state.clone()), state, token)
}

async fn owner(app: &Router, token: &str) -> (Value, Who) {
    let (status, user, cookie) = call(
        app,
        "POST",
        "/api/setup/complete",
        Some(serde_json::json!({
            "token": token,
            "email": "ada@coollabs.io",
            "password": "correct-horse-battery",
            "display_name": "Ada"
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    assert_eq!(user["workspaces"][0]["role"], "owner", "{user}");
    let workspace = user["workspaces"][0]["id"].as_str().unwrap().to_owned();
    let mut who = Who {
        cookie: cookie.expect("session cookie"),
        workspace,
        project: String::new(),
    };
    let (_, projects, _) = call(app, "GET", "/api/projects", None, Some(&who)).await;
    who.project = projects[0]["id"].as_str().unwrap().to_owned();
    (user, who)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    who: Option<&Who>,
) -> (StatusCode, Value, Option<String>) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(who) = who {
        request = request
            .header("cookie", &who.cookie)
            .header("x-owlee-workspace", &who.workspace);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    body.map(|value| value.to_string()).unwrap_or_default(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cookie = session_cookie(response.headers());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, cookie)
}

fn session_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    let pairs: Vec<&str> = headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect();
    pairs
        .iter()
        .copied()
        .find(|value| value.starts_with("owlee_session="))
        .or_else(|| pairs.first().copied())
        .map(str::to_owned)
}

#[tokio::test]
async fn sqlite_flow_covers_checks_incidents_and_the_public_page() {
    let (app, token) = app().await;
    let (status, health, _) = call(&app, "GET", "/api/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");

    let (status, _, _) = call(&app, "GET", "/api/auth/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (user, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    assert_eq!(user["email"], "ada@coollabs.io");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/health", axum::routing::get(|| async { "ok" })),
        )
        .await
        .unwrap();
    });

    let (status, monitor, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(serde_json::json!({
            "project_id": ada.project,
            "name": "API",
            "job_type": "http",
            "url": format!("http://{address}/health"),
            "interval_seconds": 30,
            "timeout_ms": 3000,
            "failure_threshold": 2,
            "assertions": [{ "type": "body", "compare": "contains", "target": "ok" }]
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{monitor}");
    let api_id = monitor["id"].as_str().unwrap().to_owned();

    let (status, checked, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{api_id}/checks"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["state"], "up");
    assert!(checked["uptime_24h"].as_f64().unwrap() > 0.99);
    assert_eq!(checked["recent"][0]["status"], "success");

    let (status, down, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(serde_json::json!({
            "project_id": ada.project,
            "name": "Worker",
            "job_type": "http",
            "url": "http://127.0.0.1:9/health",
            "timeout_ms": 1500,
            "interval_seconds": 30,
            "failure_threshold": 1
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{down}");
    let worker_id = down["id"].as_str().unwrap().to_owned();
    let (status, checked, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{worker_id}/checks"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["state"], "down");

    let (status, incidents, _) = call(&app, "GET", "/api/incidents", None, Some(&ada)).await;
    assert_eq!(status, StatusCode::OK, "{incidents}");
    assert_eq!(incidents.as_array().unwrap().len(), 1);
    assert_eq!(incidents[0]["status"], "investigating");
    assert_eq!(incidents[0]["auto"], true);
    let incident_id = incidents[0]["id"].as_str().unwrap();

    let (status, updated, _) = call(
        &app,
        "POST",
        &format!("/api/incidents/{incident_id}/updates"),
        Some(serde_json::json!({
            "status": "identified",
            "message": "The worker process is gone."
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["updates"].as_array().unwrap().len(), 2);

    let (status, page, _) = call(
        &app,
        "POST",
        "/api/pages",
        Some(serde_json::json!({
            "project_id": ada.project,
            "title": "coolLabs",
            "slug": "coollabs",
            "description": "Platform status",
            "published": true,
            "monitor_ids": [api_id, worker_id]
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{page}");

    let (status, public, _) = call(&app, "GET", "/api/public/coollabs", None, None).await;
    assert_eq!(status, StatusCode::OK, "{public}");
    assert_eq!(public["banner"], "partial_outage");
    assert_eq!(public["components"].as_array().unwrap().len(), 2);
    assert!(!public["incidents"].as_array().unwrap().is_empty());

    let starts = owlee_platform::TimestampMillis::now();
    let ends = owlee_platform::TimestampMillis::from_millis(starts.as_millis() + 3_600_000);
    let (status, maintenance, _) = call(
        &app,
        "POST",
        "/api/maintenances",
        Some(serde_json::json!({
            "project_id": ada.project,
            "title": "API restart",
            "message": "Rolling the API.",
            "page_id": page["id"],
            "monitor_ids": [api_id],
            "starts_at": starts,
            "ends_at": ends
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{maintenance}");
    let (status, checked, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{api_id}/checks"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["recent"][0]["status"], "maintenance");
    assert_eq!(checked["state"], "up");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
    let hook = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hook_addr = hook.local_addr().unwrap();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/hook",
            axum::routing::post(move |body: String| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(body).await;
                    StatusCode::NO_CONTENT
                }
            }),
        );
        axum::serve(hook, app).await.unwrap();
    });
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(hook_addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, channel, _) = call(
        &app,
        "POST",
        "/api/notifications",
        Some(serde_json::json!({
            "name": "Hooks",
            "provider": "webhook",
            "config": { "url": format!("http://{hook_addr}/hook"), "secret": "topsecret" },
            "monitor_ids": []
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{channel}");
    let channel_id = channel["id"].as_str().unwrap();
    let (status, tested, _) = call(
        &app,
        "POST",
        &format!("/api/notifications/{channel_id}/test"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    let body = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(body.contains("notification.test"), "{body}");
    assert_eq!(tested["deliveries"][0]["status"], "sent");

    let (status, _, _) = call(&app, "POST", "/api/auth/logout", None, Some(&ada)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = call(&app, "GET", "/api/auth/me", None, Some(&ada)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhooks_refuse_private_addresses_unless_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = testing_config(dir.path().join("owlee.sqlite"));
    config.allow_private_webhooks = false;
    let state = boot(config).await.unwrap();
    let token = initialize_setup_token(&state.db, TimestampMillis::now())
        .await
        .unwrap()
        .unwrap()
        .token;
    let app = router(state);
    let (_, ada) = owner(&app, &token).await;

    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/notifications",
        Some(json!({
            "name": "Metadata", "provider": "webhook", "monitor_ids": [],
            "config": { "url": "http://169.254.169.254/latest/meta-data" }
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(
        problem["fields"]["config"]
            .as_str()
            .unwrap()
            .contains("OWLEE_ALLOW_PRIVATE_WEBHOOKS"),
        "{problem}"
    );

    let (status, channel, _) = call(
        &app,
        "POST",
        "/api/notifications",
        Some(json!({
            "name": "Local", "provider": "slack", "monitor_ids": [],
            "config": { "url": "http://localhost:9/hook" }
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{channel}");
    let id = channel["id"].as_str().unwrap();
    let (status, tested, _) = call(
        &app,
        "POST",
        &format!("/api/notifications/{id}/test"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["deliveries"][0]["status"], "failed", "{tested}");
    assert!(
        tested["deliveries"][0]["error"]
            .as_str()
            .unwrap()
            .contains("OWLEE_ALLOW_PRIVATE_WEBHOOKS"),
        "the resolver refused localhost: {tested}"
    );
}

#[tokio::test]
async fn postgres_accepts_the_same_api() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| !url.is_empty())
    else {
        return;
    };
    let mut config = testing_config(std::path::PathBuf::from("data/ignored-postgres.sqlite"));
    config.database_url = Some(url);
    let state = boot(config).await.expect("postgres boot");
    let token = initialize_setup_token(&state.db, TimestampMillis::now())
        .await
        .unwrap()
        .map(|issued| issued.token);
    let editor = match token {
        Some(_) => None,
        None => Some(Who::from(editor_session(&state).await.unwrap())),
    };
    let app = router(state.clone());
    let email = format!("pg-{}@coollabs.io", owlee_platform::Id::new());
    let (status, user, _) = if let Some(token) = token {
        call(
            &app,
            "POST",
            "/api/setup/complete",
            Some(serde_json::json!({
                "token": token,
                "email": email,
                "password": "correct-horse-battery",
                "display_name": "Postgres"
            })),
            None,
        )
        .await
    } else {
        let (status, user, _) = call(&app, "GET", "/api/auth/me", None, editor.as_ref()).await;
        (status, user, None)
    };
    assert!(status.is_success(), "{status} {user}");
    let (status, ready, _) = call(&app, "GET", "/api/health/ready", None, None).await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(ready["database"], "postgres");
    postgres_runs_the_first_check_and_lists_connections(&state).await;
    detail_reports_state_age_latency_and_incidents(&state).await;
}

#[tokio::test]
async fn sign_up_rejects_the_seeded_demo_password() {
    let (app, token) = app().await;
    let (status, body, cookie) = call(
        &app,
        "POST",
        "/api/setup/complete",
        Some(serde_json::json!({
            "token": token.expect("setup token"),
            "email": "ada@coollabs.io",
            "password": DEMO_PASSWORD,
        })),
        None,
    )
    .await;
    assert!(status.is_client_error(), "{status} {body}");
    assert!(cookie.is_none());
}

#[tokio::test]
async fn failed_check_is_not_counted_when_the_host_has_no_network() {
    let (app, token) = app_with_targets(vec!["127.0.0.1:9".to_owned()]).await;
    let (_, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    let (status, monitor, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(serde_json::json!({
            "project_id": ada.project,
            "name": "Worker",
            "job_type": "tcp",
            "url": "127.0.0.1:9",
            "timeout_ms": 1000,
            "interval_seconds": 30,
            "failure_threshold": 1
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{monitor}");
    let id = monitor["id"].as_str().unwrap();

    let (status, problem, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{id}/checks"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{problem}");
    assert_eq!(problem["code"], "host_offline");

    let (_, monitor, _) = call(
        &app,
        "GET",
        &format!("/api/monitors/{id}"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(monitor["state"], "pending", "{monitor}");
    let (_, incidents, _) = call(&app, "GET", "/api/incidents", None, Some(&ada)).await;
    assert_eq!(incidents.as_array().unwrap().len(), 0, "{incidents}");
}

async fn agent_call(app: &Router, uri: &str, body: Value, token: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn agent_registers_its_own_checks_and_reports_outcomes() {
    let (app, token) = app().await;
    let (_, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    let (status, agent, _) = call(
        &app,
        "POST",
        "/api/agents",
        Some(serde_json::json!({ "project_id": ada.project, "name": "vpc-eu" })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{agent}");
    let token = agent["token"].as_str().unwrap().to_owned();
    let agent_id = agent["id"].as_str().unwrap().to_owned();
    assert_eq!(agent["online"], false);

    let checks = |names: &[&str]| {
        serde_json::json!({
            "checks": names.iter().map(|name| serde_json::json!({
                "name": name,
                "check_type": "postgres",
                "target": "10.0.0.5:5432",
                "interval_seconds": 30,
                "timeout_ms": 10000,
                "failure_threshold": 1,
            })).collect::<Vec<_>>()
        })
    };
    let (status, _) = agent_call(&app, "/api/agent/checks", checks(&["Main DB"]), "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, registered) = agent_call(
        &app,
        "/api/agent/checks",
        checks(&["Main DB", "Replica"]),
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let id = registered["checks"][0]["monitor_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let replica = registered["checks"][1]["monitor_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (_, again) = agent_call(&app, "/api/agent/checks", checks(&["Main DB"]), &token).await;
    assert_eq!(
        again["checks"][0]["monitor_id"],
        id.as_str(),
        "same name, same monitor"
    );
    let (_, monitors, _) = call(&app, "GET", "/api/monitors", None, Some(&ada)).await;
    let monitors = monitors.as_array().unwrap();
    assert_eq!(monitors.len(), 2, "{monitors:?}");
    let main = monitors
        .iter()
        .find(|monitor| monitor["id"] == id.as_str())
        .unwrap();
    assert_eq!(main["check_type"], "postgres");
    assert_eq!(main["agent_id"], agent_id.as_str());
    let dropped = monitors
        .iter()
        .find(|monitor| monitor["id"] == replica.as_str())
        .unwrap();
    assert_eq!(
        dropped["active"], false,
        "a check removed from the config is paused"
    );

    let outcome = serde_json::json!({
        "monitor_id": id,
        "status": "failure",
        "status_code": null,
        "latency_ms": 12,
        "error": "password authentication failed"
    });
    let (status, body) = agent_call(&app, "/api/agent/results", outcome.clone(), &token).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, monitor, _) = call(
        &app,
        "GET",
        &format!("/api/monitors/{id}"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(monitor["state"], "down", "{monitor}");
    assert_eq!(
        monitor["recent"][0]["error"],
        "password authentication failed"
    );
    assert_eq!(monitor["recent"][0]["region"], "vpc-eu");
    let (_, incidents, _) = call(&app, "GET", "/api/incidents", None, Some(&ada)).await;
    assert_eq!(incidents.as_array().unwrap().len(), 1, "{incidents}");

    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{id}/checks"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the server does not run agent checks"
    );
    let (status, _, _) = call(
        &app,
        "PUT",
        &format!("/api/monitors/{id}"),
        Some(serde_json::json!({ "name": "x", "job_type": "tcp", "url": "evil.internal:22" })),
        Some(&ada),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the dashboard cannot change an agent target"
    );

    let (_, other, _) = call(
        &app,
        "POST",
        "/api/agents",
        Some(serde_json::json!({ "project_id": ada.project, "name": "vpc-us" })),
        Some(&ada),
    )
    .await;
    let (status, _) = agent_call(
        &app,
        "/api/agent/results",
        outcome,
        other["token"].as_str().unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (_, agents, _) = call(&app, "GET", "/api/agents", None, Some(&ada)).await;
    let first = &agents.as_array().unwrap()[0];
    assert_eq!(first["name"], "vpc-eu", "{agents}");
    assert_eq!(first["online"], true);
    assert_eq!(first["monitor_count"], 2);
    assert!(first.get("token").is_none());

    let (status, problem, _) = call(
        &app,
        "DELETE",
        &format!("/api/agents/{agent_id}"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
}

async fn create(app: &Router, who: &Who, uri: &str, body: Value) -> String {
    let (status, created, _) = call(app, "POST", uri, Some(body), Some(who)).await;
    assert!(status.is_success(), "{uri} {status} {created}");
    created["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn another_workspace_cannot_read_change_or_link_records() {
    let (app, state, token) = app_with_state(Vec::new()).await;
    let (_, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    let eve = Who::from(editor_session(&state).await.unwrap());
    // Before the monitor, so its first check at create reaches the channels.
    let hook = |name: &str| {
        json!({
            "name": name, "provider": "webhook", "monitor_ids": [],
            "config": { "url": "http://127.0.0.1:9/hook" }
        })
    };
    let ada_channel = create(&app, &ada, "/api/notifications", hook("Ada hooks")).await;
    let eve_channel = create(&app, &eve, "/api/notifications", hook("Eve hooks")).await;

    let monitor = json!({
        "project_id": ada.project,
        "name": "Worker",
        "job_type": "tcp",
        "url": "127.0.0.1:9",
        "timeout_ms": 1000,
        "failure_threshold": 1
    });
    let monitor_id = create(&app, &ada, "/api/monitors", monitor.clone()).await;
    let page_id = create(
        &app,
        &ada,
        "/api/pages",
        json!({"project_id": ada.project, "title": "Ada", "slug": "ada", "monitor_ids": [monitor_id]}),
    )
    .await;
    let incident_id = create(
        &app,
        &ada,
        "/api/incidents",
        json!({
            "project_id": ada.project, "title": "Slow", "status": "investigating",
            "impact": "minor", "message": "Looking", "page_id": page_id, "monitor_id": monitor_id
        }),
    )
    .await;

    let page = json!({"title": "Mine", "slug": "mine", "monitor_ids": []});
    let foreign: Vec<(&str, String, Option<Value>)> = vec![
        ("GET", format!("/api/monitors/{monitor_id}"), None),
        ("PUT", format!("/api/monitors/{monitor_id}"), Some(monitor)),
        ("POST", format!("/api/monitors/{monitor_id}/checks"), None),
        (
            "POST",
            format!("/api/monitors/{monitor_id}/active"),
            Some(json!({"active": false})),
        ),
        ("DELETE", format!("/api/monitors/{monitor_id}"), None),
        ("GET", format!("/api/pages/{page_id}"), None),
        ("PUT", format!("/api/pages/{page_id}"), Some(page)),
        ("DELETE", format!("/api/pages/{page_id}"), None),
        ("GET", format!("/api/incidents/{incident_id}"), None),
        (
            "POST",
            format!("/api/incidents/{incident_id}/updates"),
            Some(json!({"status": "resolved", "message": "Mine now"})),
        ),
        ("DELETE", format!("/api/incidents/{incident_id}"), None),
        (
            "PUT",
            format!("/api/notifications/{ada_channel}"),
            Some(hook("Taken")),
        ),
        (
            "POST",
            format!("/api/notifications/{ada_channel}/test"),
            None,
        ),
        ("DELETE", format!("/api/notifications/{ada_channel}"), None),
        (
            "POST",
            "/api/monitors".into(),
            Some(
                json!({"project_id": ada.project, "name": "x", "job_type": "tcp", "url": "127.0.0.1:9"}),
            ),
        ),
        (
            "GET",
            format!("/api/monitors?project={}", ada.project),
            None,
        ),
    ];
    for (method, uri, body) in foreign {
        let (status, problem, _) = call(&app, method, &uri, body, Some(&eve)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri} {problem}");
    }

    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/pages",
        Some(json!({"project_id": eve.project, "title": "Eve", "slug": "eve", "monitor_ids": [monitor_id]})),
        Some(&eve),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(problem["fields"]["monitor_ids"].is_string(), "{problem}");
    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/incidents",
        Some(json!({
            "project_id": eve.project, "title": "Eve", "status": "investigating",
            "impact": "minor", "message": "x", "page_id": page_id
        })),
        Some(&eve),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(problem["fields"]["page_id"].is_string(), "{problem}");
    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/notifications",
        Some(json!({
            "name": "Spy", "provider": "webhook", "monitor_ids": [monitor_id],
            "config": { "url": "http://127.0.0.1:9/hook" }
        })),
        Some(&eve),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");

    let outsider = Who {
        cookie: eve.cookie.clone(),
        workspace: ada.workspace.clone(),
        project: String::new(),
    };
    let (status, _, _) = call(&app, "GET", "/api/monitors", None, Some(&outsider)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a header naming another workspace"
    );

    for uri in [
        "/api/monitors",
        "/api/pages",
        "/api/incidents",
        "/api/overview",
    ] {
        let (status, listed, _) = call(&app, "GET", uri, None, Some(&eve)).await;
        assert_eq!(status, StatusCode::OK, "{uri} {listed}");
        let records = listed.get("monitors").unwrap_or(&listed);
        assert_eq!(records, &json!([]), "{uri}");
    }
    let (_, overview, _) = call(&app, "GET", "/api/overview", None, Some(&eve)).await;
    let actions = overview["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["action"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        ["notification.created"],
        "only Eve's own audit trail"
    );

    let (status, checked, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{monitor_id}/checks"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["state"], "down");
    let (_, ada_channels, _) = call(&app, "GET", "/api/notifications", None, Some(&ada)).await;
    assert_eq!(ada_channels[0]["id"], ada_channel.as_str());
    let events = ada_channels[0]["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|delivery| delivery["event"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events, ["incident.updated", "monitor.down"]);
    let (_, eve_channels, _) = call(&app, "GET", "/api/notifications", None, Some(&eve)).await;
    assert_eq!(eve_channels[0]["id"], eve_channel.as_str());
    assert_eq!(
        eve_channels[0]["deliveries"],
        json!([]),
        "Ada's outage never reaches Eve's channel"
    );
}

#[tokio::test]
async fn project_filter_lists_one_project_and_members_cannot_create() {
    let (app, state, token) = app_with_state(Vec::new()).await;
    let (_, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    let api = create(
        &app,
        &ada,
        "/api/projects",
        json!({"name": "API", "color": "#0ea5e9"}),
    )
    .await;
    let monitor = |project: &str, name: &str| json!({"project_id": project, "name": name, "job_type": "tcp", "url": "127.0.0.1:9"});
    let website_monitor = create(&app, &ada, "/api/monitors", monitor(&ada.project, "Site")).await;
    let api_monitor = create(&app, &ada, "/api/monitors", monitor(&api, "Gateway")).await;

    let ids = |listed: &Value| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .map(|monitor| monitor["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let (_, listed, _) = call(
        &app,
        "GET",
        &format!("/api/monitors?project={api}"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(ids(&listed), [api_monitor.clone()]);
    assert_eq!(listed[0]["project_id"], api.as_str());
    let (_, listed, _) = call(&app, "GET", "/api/monitors", None, Some(&ada)).await;
    assert_eq!(ids(&listed), [api_monitor, website_monitor]);
    let (status, _, _) = call(&app, "GET", "/api/monitors?project=nope", None, Some(&ada)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({"name": "No project", "job_type": "tcp", "url": "127.0.0.1:9"})),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(problem["fields"]["project_id"].is_string(), "{problem}");

    let workspace = ada.workspace.parse().unwrap();
    let member = Who::from(member_session(&state, workspace).await.unwrap());
    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(monitor(&ada.project, "Member")),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let (status, listed, _) = call(&app, "GET", "/api/monitors", None, Some(&member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 2);
}

async fn local_health() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/health", axum::routing::get(|| async { "ok" })),
        )
        .await
        .unwrap();
    });
    address
}

async fn count(state: &AppState, table: &str) -> i64 {
    use sqlx::Row;
    sqlx::query(&format!("SELECT COUNT(*) AS n FROM {table}"))
        .fetch_one(state.db.pool())
        .await
        .unwrap()
        .get::<i64, _>("n")
}

#[tokio::test]
async fn monitor_test_runs_one_check_and_stores_nothing() {
    let (app, state, token) = app_with_state(Vec::new()).await;
    let (_, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    let address = local_health().await;
    let test = |job_type: &str, url: String| json!({"project_id": ada.project, "name": "Try", "job_type": job_type, "url": url, "timeout_ms": 2000});

    let (status, up, _) = call(
        &app,
        "POST",
        "/api/monitors/test",
        Some(test("http", format!("http://{address}/health"))),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{up}");
    assert_eq!(up["status"], "success");
    assert_eq!(up["status_code"], 200);
    assert_eq!(up["error"], Value::Null);
    assert!(up["latency_ms"].is_i64(), "{up}");

    let (status, down, _) = call(
        &app,
        "POST",
        "/api/monitors/test",
        Some(test("tcp", "127.0.0.1:9".into())),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{down}");
    assert_eq!(down["status"], "failure");
    assert_eq!(down["status_code"], Value::Null);
    assert_eq!(down["error"], "Connection refused (os error 111)");

    assert_eq!(count(&state, "check_results").await, 0);
    assert_eq!(count(&state, "monitors").await, 0);

    let mut statuses = Vec::new();
    for _ in 0..14 {
        let (status, _, _) = call(
            &app,
            "POST",
            "/api/monitors/test",
            Some(test("tcp", "127.0.0.1:9".into())),
            Some(&ada),
        )
        .await;
        statuses.push(status.as_u16());
    }
    assert_eq!(
        statuses,
        [[200; 13].as_slice(), &[429]].concat(),
        "a burst of 15 checks per user, shared with Run check"
    );

    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/monitors/test",
        Some(test("tcp", "no port".into())),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(problem["fields"]["url"].is_string(), "{problem}");

    let workspace = ada.workspace.parse().unwrap();
    let member = Who::from(member_session(&state, workspace).await.unwrap());
    let (status, problem, _) = call(
        &app,
        "POST",
        "/api/monitors/test",
        Some(test("tcp", "127.0.0.1:9".into())),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
}

#[tokio::test]
async fn create_runs_the_first_check_and_detail_lists_connections() {
    let (app, state, token) = app_with_state(Vec::new()).await;
    let (_, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    let eve = Who::from(editor_session(&state).await.unwrap());
    let address = local_health().await;
    let hook = |name: &str| {
        json!({
            "name": name, "provider": "webhook", "monitor_ids": [],
            "config": { "url": "http://127.0.0.1:9/hook" }
        })
    };
    let ada_channel = create(&app, &ada, "/api/notifications", hook("Ops hooks")).await;
    create(&app, &eve, "/api/notifications", hook("Eve hooks")).await;

    let (status, api, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "project_id": ada.project, "name": "API", "job_type": "http",
            "url": format!("http://{address}/health"), "timeout_ms": 2000
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{api}");
    assert_eq!(api["state"], "up");
    assert_eq!(api["recent"].as_array().unwrap().len(), 1);
    assert_eq!(api["recent"][0]["status"], "success");
    assert_eq!(api["open_incident_id"], Value::Null);
    let api_id = api["id"].as_str().unwrap().to_owned();

    let page_id = create(
        &app,
        &ada,
        "/api/pages",
        json!({"project_id": ada.project, "title": "Ada status", "slug": "ada", "monitor_ids": [api_id]}),
    )
    .await;
    let eve_page = create(
        &app,
        &eve,
        "/api/pages",
        json!({"project_id": eve.project, "title": "Eve status", "slug": "eve", "monitor_ids": []}),
    )
    .await;
    sqlx::query(
        "INSERT INTO page_components (id, page_id, monitor_id, name, created_at) VALUES ($1, $2, $3, 'API', 0)",
    )
    .bind(owlee_platform::Id::new().to_string())
    .bind(&eve_page)
    .bind(&api_id)
    .execute(state.db.pool())
    .await
    .unwrap();

    let (status, detail, _) = call(
        &app,
        "GET",
        &format!("/api/monitors/{api_id}"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["alert_channels"],
        json!([{"id": ada_channel, "name": "Ops hooks", "provider": "webhook"}])
    );
    assert_eq!(detail["alert_channel_count"], 1);
    assert_eq!(
        detail["status_pages"],
        json!([{"id": page_id, "title": "Ada status", "slug": "ada"}]),
        "Eve's page is in another workspace"
    );
    assert_eq!(detail["open_incident_id"], Value::Null);

    let workspace = ada.workspace.parse().unwrap();
    let member = Who::from(member_session(&state, workspace).await.unwrap());
    let (_, seen, _) = call(
        &app,
        "GET",
        &format!("/api/monitors/{api_id}"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(
        seen["alert_channels"],
        json!([]),
        "members cannot read channels"
    );
    assert_eq!(seen["alert_channel_count"], 1);
    assert_eq!(seen["status_pages"], detail["status_pages"]);

    let (status, worker, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "project_id": ada.project, "name": "Worker", "job_type": "tcp",
            "url": "127.0.0.1:9", "timeout_ms": 1000, "failure_threshold": 1
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{worker}");
    assert_eq!(worker["state"], "down");
    let (_, incidents, _) = call(&app, "GET", "/api/incidents", None, Some(&ada)).await;
    assert_eq!(incidents.as_array().unwrap().len(), 1, "{incidents}");
    assert_eq!(worker["open_incident_id"], incidents[0]["id"]);
    assert_eq!(count(&state, "check_results").await, 2);
}

/// Part of `postgres_accepts_the_same_api`: two tests booting one empty
/// database at once race on the migrations.
async fn postgres_runs_the_first_check_and_lists_connections(state: &AppState) {
    let ada = Who::from(editor_session(state).await.unwrap());
    let app = router(state.clone());
    let address = local_health().await;

    let body = json!({
        "project_id": ada.project, "name": "API", "job_type": "http",
        "url": format!("http://{address}/health"), "timeout_ms": 2000
    });
    let (status, tested, _) = call(
        &app,
        "POST",
        "/api/monitors/test",
        Some(body.clone()),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["status"], "success");

    let (status, api, _) = call(&app, "POST", "/api/monitors", Some(body), Some(&ada)).await;
    assert_eq!(status, StatusCode::CREATED, "{api}");
    let api_id = api["id"].as_str().unwrap().to_owned();
    assert_eq!(api["recent"][0]["status"], "success", "{api}");
    let slug = format!("pg-{}", &api_id[api_id.len() - 12..]);
    let page_id = create(
        &app,
        &ada,
        "/api/pages",
        json!({"project_id": ada.project, "title": "PG status", "slug": slug, "monitor_ids": [api_id]}),
    )
    .await;
    let (_, detail, _) = call(
        &app,
        "GET",
        &format!("/api/monitors/{api_id}"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(
        detail["status_pages"],
        json!([{"id": page_id, "title": "PG status", "slug": slug}])
    );
    assert_eq!(detail["alert_channels"], json!([]));
    assert_eq!(detail["open_incident_id"], Value::Null);
}

#[tokio::test]
async fn sqlite_detail_reports_state_age_latency_and_incidents() {
    let (_, state, _) = app_with_state(Vec::new()).await;
    detail_reports_state_age_latency_and_incidents(&state).await;
}

async fn exec(state: &AppState, sql: &str, binds: &[&str]) {
    let mut query = sqlx::query(sql);
    for value in binds {
        query = query.bind(*value);
    }
    query.execute(state.db.pool()).await.unwrap();
}

async fn state_changed_at(app: &Router, who: &Who, id: &str) -> Value {
    let (status, detail, _) =
        call(app, "GET", &format!("/api/monitors/{id}"), None, Some(who)).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    detail["state_changed_at"].clone()
}

/// Runs on SQLite and, from `postgres_accepts_the_same_api`, on Postgres.
async fn detail_reports_state_age_latency_and_incidents(state: &AppState) {
    let ada = Who::from(editor_session(state).await.unwrap());
    let eve = Who::from(editor_session(state).await.unwrap());
    let app = router(state.clone());
    let pin = |id: &str, at: &str| {
        format!("UPDATE monitors SET state_changed_at = {at} WHERE id = '{id}'")
    };

    let (status, worker, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "project_id": ada.project, "name": "Worker", "job_type": "tcp",
            "url": "127.0.0.1:9", "timeout_ms": 1000, "failure_threshold": 2
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{worker}");
    assert_eq!(
        worker["state"], "pending",
        "one failure is under the threshold"
    );
    assert_eq!(worker["state_changed_at"], worker["created_at"]);
    let worker_id = worker["id"].as_str().unwrap().to_owned();
    let run = format!("/api/monitors/{worker_id}/checks");
    let active = format!("/api/monitors/{worker_id}/active");

    exec(state, &pin(&worker_id, "1000"), &[]).await;
    let (_, down, _) = call(&app, "POST", &run, None, Some(&ada)).await;
    assert_eq!(down["state"], "down", "{down}");
    assert_ne!(
        down["state_changed_at"], "1970-01-01T00:00:01.000Z",
        "pending to down moves it"
    );
    assert_eq!(down["consecutive_failures"], 2);
    assert_eq!(
        down["last_error"]["error"],
        "Connection refused (os error 111)"
    );
    assert_eq!(
        down["latency_24h"],
        Value::Null,
        "failures have no answered latency"
    );

    exec(state, &pin(&worker_id, "2000"), &[]).await;
    let (_, still, _) = call(&app, "POST", &run, None, Some(&ada)).await;
    assert_eq!(still["state"], "down");
    assert_eq!(
        still["state_changed_at"], "1970-01-01T00:00:02.000Z",
        "down to down keeps it"
    );

    let (_, paused, _) = call(
        &app,
        "POST",
        &active,
        Some(json!({"active": false})),
        Some(&ada),
    )
    .await;
    assert_eq!(paused["active"], false);
    assert_ne!(
        paused["state_changed_at"], "1970-01-01T00:00:02.000Z",
        "pausing moves it"
    );
    exec(state, &pin(&worker_id, "3000"), &[]).await;
    call(
        &app,
        "POST",
        &active,
        Some(json!({"active": false})),
        Some(&ada),
    )
    .await;
    assert_eq!(
        state_changed_at(&app, &ada, &worker_id).await,
        "1970-01-01T00:00:03.000Z",
        "pausing twice keeps it"
    );
    call(
        &app,
        "POST",
        &active,
        Some(json!({"active": true})),
        Some(&ada),
    )
    .await;
    assert_ne!(
        state_changed_at(&app, &ada, &worker_id).await,
        "1970-01-01T00:00:03.000Z",
        "resuming moves it"
    );

    let (status, api, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "project_id": ada.project, "name": "API", "job_type": "http",
            "url": "http://127.0.0.1:9/", "active": false
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{api}");
    assert_eq!(
        api["recent"],
        Value::Null,
        "a paused monitor runs no first check"
    );
    assert_eq!(api["latency_24h"], Value::Null);
    assert_eq!(api["last_error"], Value::Null);
    assert_eq!(api["incidents"], json!([]));
    let api_id = api["id"].as_str().unwrap().to_owned();

    let now = TimestampMillis::now().as_millis();
    let mut rows = (1..=20)
        .map(|n| ("success", n * 10, None, now - n * 60_000))
        .collect::<Vec<_>>();
    rows.extend([
        (
            "degraded",
            500,
            Some("latency 500ms is above the degraded threshold of 400ms"),
            now - 30 * 60_000,
        ),
        ("failure", 9_000, Some("timed out"), now - 40 * 60_000),
        ("maintenance", 0, None, now - 50 * 60_000),
        ("success", 7_000, None, now - 25 * 3_600_000),
    ]);
    for (status, latency, error, at) in rows {
        sqlx::query(
            "INSERT INTO check_results (id, monitor_id, status, status_code, latency_ms, error, region, checked_at) \
             VALUES ($1, $2, $3, NULL, $4, $5, 'local', $6)",
        )
        .bind(owlee_platform::Id::new().to_string())
        .bind(&api_id)
        .bind(status)
        .bind(latency)
        .bind(error)
        .bind(at)
        .execute(state.db.pool())
        .await
        .unwrap();
    }

    let insert_incident = |project: &str, n: i64| {
        let (id, project, api_id) = (
            owlee_platform::Id::new().to_string(),
            project.to_owned(),
            api_id.clone(),
        );
        async move {
            sqlx::query(
                "INSERT INTO incidents (id, project_id, monitor_id, title, status, impact, auto, started_at, resolved_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, 'major', 1, $6, $7, $6, $6)",
            )
            .bind(&id)
            .bind(&project)
            .bind(&api_id)
            .bind(format!("Incident {n}"))
            .bind(if n == 12 { "investigating" } else { "resolved" })
            .bind(n * 1000)
            .bind(if n == 12 { None } else { Some(n * 1000 + 500) })
            .execute(state.db.pool())
            .await
            .unwrap();
            id
        }
    };
    let mut newest = String::new();
    for n in 1..=12 {
        let id = insert_incident(&ada.project, n).await;
        if n == 12 {
            newest = id;
        }
    }
    insert_incident(&eve.project, 99).await;

    let (status, detail, _) = call(
        &app,
        "GET",
        &format!("/api/monitors/{api_id}"),
        None,
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["latency_24h"],
        json!({"p50": 110, "p95": 200, "max": 500}),
        "20 successes of 10..200 ms and one degraded 500 ms; the failure, maintenance and 25 h old rows are left out"
    );
    assert_eq!(
        detail["last_error"]["error"],
        "latency 500ms is above the degraded threshold of 400ms"
    );
    let titles = detail["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|incident| incident["title"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        [
            "Incident 12",
            "Incident 11",
            "Incident 10",
            "Incident 9",
            "Incident 8",
            "Incident 7",
            "Incident 6",
            "Incident 5",
            "Incident 4",
            "Incident 3"
        ],
        "newest 10, and not the incident in Eve's workspace that names the same monitor"
    );
    assert_eq!(
        detail["incidents"][0],
        json!({"id": newest, "title": "Incident 12", "status": "investigating", "started_at": "1970-01-01T00:00:12.000Z", "resolved_at": null})
    );
    assert_eq!(
        detail["incidents"][1]["resolved_at"],
        "1970-01-01T00:00:11.500Z"
    );

    let (_, listed, _) = call(&app, "GET", "/api/monitors", None, Some(&ada)).await;
    assert_eq!(listed[0]["name"], "API");
    assert_eq!(listed[0]["state_changed_at"], api["state_changed_at"]);
    assert_eq!(listed[0]["latency_24h"], Value::Null, "the list stays lean");
}

#[tokio::test]
async fn monitor_url_secrets_stay_out_of_incidents_errors_dashboard_and_alerts() {
    use sqlx::Row;

    const SECRET_URL: &str = "http://usr-LEAK0:pw-LEAK1@127.0.0.1:9/health?token=LEAK2";
    let (app, state, token) = app_with_state(Vec::new()).await;
    let (_, ada) = owner(&app, token.as_deref().expect("setup token")).await;
    let member = Who::from(
        member_session(&state, ada.workspace.parse().unwrap())
            .await
            .unwrap(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    let hook = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hook_addr = hook.local_addr().unwrap();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/hook",
            axum::routing::post(move |body: String| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(body).await;
                    StatusCode::NO_CONTENT
                }
            }),
        );
        axum::serve(hook, app).await.unwrap();
    });
    for provider in ["webhook", "slack"] {
        create(
            &app,
            &ada,
            "/api/notifications",
            json!({
                "name": provider, "provider": provider, "monitor_ids": [],
                "config": { "url": format!("http://{hook_addr}/hook") }
            }),
        )
        .await;
    }

    let (status, monitor, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "project_id": ada.project, "name": "Billing API", "job_type": "http",
            "url": SECRET_URL, "timeout_ms": 2000, "failure_threshold": 1
        })),
        Some(&ada),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{monitor}");
    assert_eq!(monitor["state"], "down", "{monitor}");
    let id = monitor["id"].as_str().unwrap().to_owned();
    let incident_id = monitor["open_incident_id"].as_str().unwrap().to_owned();
    create(
        &app,
        &ada,
        "/api/pages",
        json!({"project_id": ada.project, "title": "Billing", "slug": "billing", "published": true, "monitor_ids": [id]}),
    )
    .await;

    let mut surfaces: Vec<(String, String)> = Vec::new();
    for _ in 0..2 {
        let body = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("both channels are notified")
            .unwrap();
        let kind = if body.contains("\"event\"") {
            "webhook payload"
        } else {
            "slack text"
        };
        surfaces.push((kind.to_owned(), body));
    }

    let detail = format!("/api/monitors/{id}");
    let (status, editor_view, _) = call(&app, "GET", &detail, None, Some(&ada)).await;
    assert_eq!(status, StatusCode::OK, "{editor_view}");
    assert_eq!(editor_view["url"], SECRET_URL, "editors keep the full URL");
    surfaces.push((
        "editor monitor last_error".to_owned(),
        editor_view["last_error"].to_string(),
    ));
    surfaces.push((
        "editor monitor recent".to_owned(),
        editor_view["recent"].to_string(),
    ));

    let (status, public, _) = call(&app, "GET", "/api/public/billing", None, None).await;
    assert_eq!(status, StatusCode::OK, "{public}");
    assert_eq!(public["incidents"].as_array().unwrap().len(), 1, "{public}");
    surfaces.push(("public status page".to_owned(), public.to_string()));

    for (name, who, uri) in [
        ("member monitor", &member, detail.clone()),
        ("member monitor list", &member, "/api/monitors".to_owned()),
        (
            "member incident",
            &member,
            format!("/api/incidents/{incident_id}"),
        ),
        ("member incident list", &member, "/api/incidents".to_owned()),
        ("member dashboard", &member, "/api/dashboard".to_owned()),
        ("editor dashboard", &ada, "/api/dashboard".to_owned()),
        (
            "editor incident",
            &ada,
            format!("/api/incidents/{incident_id}"),
        ),
    ] {
        let (status, body, _) = call(&app, "GET", &uri, None, Some(who)).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
        surfaces.push((name.to_owned(), body.to_string()));
    }

    let errors = sqlx::query("SELECT error FROM check_results")
        .fetch_all(state.db.pool())
        .await
        .unwrap();
    assert_eq!(errors.len(), 1);
    for row in errors {
        surfaces.push((
            "check_results.error".to_owned(),
            row.get::<Option<String>, _>("error")
                .expect("the failed check stores an error"),
        ));
    }

    let leaks: Vec<String> = surfaces
        .iter()
        .flat_map(|(name, text)| {
            ["LEAK0", "LEAK1", "LEAK2"]
                .into_iter()
                .filter(move |secret| text.contains(secret))
                .map(move |secret| format!("{name}: {secret}"))
        })
        .collect();
    assert_eq!(leaks, Vec::<String>::new(), "monitor URL secrets leaked");

    for (kind, body) in &surfaces[..2] {
        assert!(
            body.contains("Billing API") && body.contains("http://127.0.0.1:9/health"),
            "{kind}: {body}"
        );
    }
    assert_eq!(
        public["incidents"][0]["updates"][0]["message"],
        "Owlee recorded 1 consecutive failed checks for Billing API. error sending request: \
         client error (Connect): tcp connect error: Connection refused (os error 111)",
        "{public}"
    );
    let (_, dashboard, _) = call(&app, "GET", "/api/dashboard", None, Some(&ada)).await;
    assert_eq!(
        dashboard["monitors"]["failing"][0]["url"], "http://127.0.0.1:9/health",
        "{dashboard}"
    );
}
