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
