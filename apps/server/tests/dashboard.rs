use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_server::{AppState, TestSession, boot, editor_session, router, testing_config};
use serde_json::{Value, json};
use tower::ServiceExt;

const BROWSER: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36";

async fn app() -> (Router, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    let state = boot(testing_config(path)).await.unwrap();
    (router(state.clone()), state)
}

/// Sends `session`'s cookie and workspace header when given.
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    session: Option<&TestSession>,
    extra: &[(&str, &str)],
) -> (StatusCode, Value, Option<String>, Option<String>) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-owlee-workspace", session.workspace_id.to_string());
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
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
    let headers = response.headers().clone();
    let cookie = session_cookie(&headers);
    let location = headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, cookie, location)
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

fn hex_id() -> String {
    owlee_platform::Id::new().to_string().replace('-', "")
}

/// An error app in `project_id` with one stored event.
async fn error_app_with_event(app: &Router, session: &TestSession, project_id: &str, name: &str) {
    let (status, created, _, _) = call(
        app,
        "POST",
        "/api/error-apps",
        Some(json!({"name": name, "platform": "php", "project_id": project_id})),
        Some(session),
        &[],
    )
    .await;
    assert!(status.is_success(), "{status} {created}");
    let app_id = created["id"].as_str().unwrap();
    let key = created["publicKey"].as_str().unwrap();
    let (status, stored, _, _) = call(
        app,
        "POST",
        &format!("/api/{app_id}/store/"),
        Some(json!({
            "event_id": hex_id(),
            "level": "error",
            "platform": "php",
            "exception": {
                "values": [{
                    "type": "RuntimeException",
                    "value": format!("disk full in {name}")
                }]
            }
        })),
        None,
        &[(
            "authorization",
            &format!("Sentry sentry_key={key}, sentry_version=7"),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stored}");
}

#[tokio::test]
async fn dashboard_aggregates_monitors_errors_and_traffic() {
    let (app, state) = app().await;
    let (status, _, _, _) = call(&app, "GET", "/api/dashboard", None, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = editor_session(&state).await.unwrap();
    let project_id = cookie.project_id.to_string();
    let (status, empty, _, _) = call(&app, "GET", "/api/dashboard", None, Some(&cookie), &[]).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["monitors"]["up"], 0);
    assert_eq!(empty["monitors"]["down"], 0);
    assert_eq!(empty["monitors"]["degraded"], 0);
    assert_eq!(empty["monitors"]["pending"], 0);
    assert_eq!(empty["monitors"]["paused"], 0);
    assert!(empty["monitors"]["uptime_today"].is_null());
    assert!(empty["monitors"]["failing"].as_array().unwrap().is_empty());
    assert_eq!(empty["incidents"]["open"], 0);
    assert!(empty["pages"].as_array().unwrap().is_empty());
    assert_eq!(empty["errors"]["apps"], 0);
    assert_eq!(empty["errors"]["unresolved"], 0);
    assert_eq!(empty["errors"]["new_24h"], 0);
    assert_eq!(empty["errors"]["events_24h"].as_array().unwrap().len(), 24);
    assert!(
        empty["errors"]["events_24h"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value.as_i64() == Some(0))
    );
    assert!(empty["errors"]["issues"].as_array().unwrap().is_empty());
    assert_eq!(empty["analytics"]["sites"], 0);
    assert_eq!(empty["analytics"]["links"], 0);
    assert_eq!(empty["analytics"]["days"], 7);
    assert_eq!(empty["analytics"]["pageviews"], 0);
    assert_eq!(empty["analytics"]["visitors"], 0);
    assert_eq!(empty["analytics"]["clicks"], 0);
    assert_eq!(empty["analytics"]["previous_pageviews"], 0);

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

    let (status, monitor, _, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "name": "API",
            "job_type": "http",
            "url": format!("http://{address}/health"),
            "interval_seconds": 30,
            "timeout_ms": 3000,
            "failure_threshold": 1,
            "project_id": project_id
        })),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{monitor}");
    let api_id = monitor["id"].as_str().unwrap();
    let (status, checked, _, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{api_id}/checks"),
        None,
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["state"], "up");

    let (status, down, _, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "name": "Worker",
            "job_type": "http",
            "url": "http://127.0.0.1:9/health",
            "timeout_ms": 1000,
            "interval_seconds": 30,
            "failure_threshold": 1,
            "project_id": project_id
        })),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{down}");
    let worker_id = down["id"].as_str().unwrap();
    let (status, checked, _, _) = call(
        &app,
        "POST",
        &format!("/api/monitors/{worker_id}/checks"),
        None,
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["state"], "down");

    let (status, page, _, _) = call(
        &app,
        "POST",
        "/api/pages",
        Some(json!({
            "title": "coolLabs",
            "slug": "coollabs",
            "published": true,
            "monitor_ids": [api_id, worker_id],
            "project_id": project_id
        })),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{page}");

    error_app_with_event(&app, &cookie, &project_id, "Checkout").await;

    let (status, site, _, _) = call(
        &app,
        "POST",
        "/api/sites",
        Some(json!({"name": "Docs", "domain": "coolify.io", "project_id": project_id})),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{site}");
    let (status, _, _, _) = call(
        &app,
        "POST",
        "/api/receive",
        Some(json!({
            "site": site["public_id"],
            "type": "pageview",
            "url": "https://coolify.io/docs",
            "title": "Docs"
        })),
        None,
        &[("user-agent", BROWSER), ("origin", "https://coolify.io")],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, domain, _, _) = call(
        &app,
        "POST",
        "/api/domains",
        Some(json!({"hostname": "go.owlee.test", "enabled": true})),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{domain}");
    let (status, link, _, _) = call(
        &app,
        "POST",
        "/api/links",
        Some(json!({
            "domain_id": domain["id"],
            "slug": "coolify",
            "destination": "https://coolify.io",
            "redirect_type": "temporary",
            "project_id": project_id
        })),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let (status, _, _, location) = call(
        &app,
        "GET",
        "/go/coolify",
        None,
        None,
        &[("user-agent", BROWSER)],
    )
    .await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location.as_deref(), Some("https://coolify.io"));

    let (status, body, _, _) = call(&app, "GET", "/api/dashboard", None, Some(&cookie), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["monitors"]["up"], 1);
    assert_eq!(body["monitors"]["down"], 1);
    assert_eq!(body["monitors"]["degraded"], 0);
    let uptime = body["monitors"]["uptime_today"].as_f64().unwrap();
    assert!((uptime - 0.5).abs() < 0.01, "{uptime}");
    assert!(body["monitors"]["avg_latency_ms"].as_i64().unwrap() >= 0);
    let failing = body["monitors"]["failing"].as_array().unwrap();
    assert_eq!(failing.len(), 1, "{body}");
    assert_eq!(failing[0]["name"], "Worker");
    assert_eq!(failing[0]["state"], "down");
    assert!(failing[0].get("bars").is_none());
    assert_eq!(failing[0]["uptime_today"].as_f64().unwrap(), 0.0);
    assert_eq!(body["incidents"]["open"], 1);
    let incident = &body["incidents"]["items"][0];
    assert_eq!(incident["title"], "Worker is down");
    assert_eq!(incident["status"], "investigating");
    assert_eq!(failing[0]["since"], incident["started_at"]);
    assert_eq!(body["pages"].as_array().unwrap().len(), 1);
    assert_eq!(body["pages"][0]["title"], "coolLabs");
    assert_eq!(body["pages"][0]["banner"], "partial_outage");
    assert_eq!(body["pages"][0]["published"], true);
    assert_eq!(body["errors"]["apps"], 1);
    assert!(body["errors"]["unresolved"].as_i64().unwrap() >= 1);
    assert!(body["errors"]["new_24h"].as_i64().unwrap() >= 1);
    assert_eq!(body["errors"]["events_24h"].as_array().unwrap().len(), 24);
    let events: i64 = body["errors"]["events_24h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_i64().unwrap())
        .sum();
    assert!(events >= 1, "{body}");
    assert!(body["errors"]["issues"][0]["event_count"].as_i64().unwrap() >= 1);
    assert!(
        body["errors"]["issues"][0]["title"]
            .as_str()
            .unwrap()
            .contains("disk full")
    );
    assert_eq!(body["errors"]["issues"][0]["app_name"], "Checkout");
    assert_eq!(body["analytics"]["sites"], 1);
    assert_eq!(body["analytics"]["links"], 1);
    assert!(body["analytics"]["pageviews"].as_i64().unwrap() >= 1);
    assert!(body["analytics"]["visitors"].as_i64().unwrap() >= 1);
    assert_eq!(body["analytics"]["previous_pageviews"], 0);
    assert!(body["analytics"]["clicks"].as_i64().unwrap() >= 1);
    assert_eq!(body["analytics"]["previous_clicks"], 0);
    assert_eq!(body["analytics"]["top_sites"][0]["name"], "Docs");
    assert!(
        body["analytics"]["top_sites"][0]["pageviews"]
            .as_i64()
            .unwrap()
            >= 1
    );
}

#[tokio::test]
async fn postgres_dashboard_query_runs_when_configured() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| !url.is_empty())
    else {
        return;
    };
    let mut config = testing_config(std::path::PathBuf::from(
        "data/ignored-dashboard-postgres.sqlite",
    ));
    config.database_url = Some(url);
    let state = boot(config).await.expect("postgres boot");
    let session = editor_session(&state).await.unwrap();
    let app = router(state);
    let (status, body, _, _) = call(&app, "GET", "/api/dashboard", None, Some(&session), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["analytics"]["days"], 7);
    assert_eq!(body["errors"]["events_24h"].as_array().unwrap().len(), 24);
    assert!(body["monitors"]["failing"].is_array());
    assert!(body["pages"].is_array());
}

/// `?project=` narrows every section. Another workspace sees none of it.
#[tokio::test]
async fn dashboard_is_scoped_to_workspace_and_project() {
    let (app, state) = app().await;
    let owner = editor_session(&state).await.unwrap();
    let default_project = owner.project_id.to_string();
    let (status, mobile, _, _) = call(
        &app,
        "POST",
        "/api/projects",
        Some(json!({"name": "Mobile"})),
        Some(&owner),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{mobile}");
    let mobile_project = mobile["id"].as_str().unwrap().to_owned();
    error_app_with_event(&app, &owner, &default_project, "Web").await;
    error_app_with_event(&app, &owner, &mobile_project, "iOS").await;
    let (status, monitor, _, _) = call(
        &app,
        "POST",
        "/api/monitors",
        Some(json!({
            "name": "Paused API",
            "job_type": "http",
            "url": "http://127.0.0.1:9/health",
            "interval_seconds": 30,
            "timeout_ms": 1000,
            "active": false,
            "project_id": default_project
        })),
        Some(&owner),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{monitor}");

    let dashboard = |query: String, session: TestSession| {
        let app = app.clone();
        async move {
            let (status, body, _, _) = call(
                &app,
                "GET",
                &format!("/api/dashboard{query}"),
                None,
                Some(&session),
                &[],
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body
        }
    };
    let clone = |session: &TestSession| TestSession {
        cookie: session.cookie.clone(),
        user_id: session.user_id,
        workspace_id: session.workspace_id,
        project_id: session.project_id,
    };
    let monitor_total = |body: &Value| -> i64 {
        ["up", "down", "degraded", "pending", "paused"]
            .iter()
            .map(|key| body["monitors"][key].as_i64().unwrap())
            .sum()
    };

    let all = dashboard(String::new(), clone(&owner)).await;
    assert_eq!(all["errors"]["apps"], 2);
    assert_eq!(all["errors"]["unresolved"], 2);
    assert_eq!(monitor_total(&all), 1, "{all}");

    let narrowed = dashboard(format!("?project={mobile_project}"), clone(&owner)).await;
    assert_eq!(narrowed["errors"]["apps"], 1);
    assert_eq!(narrowed["errors"]["unresolved"], 1);
    assert_eq!(narrowed["errors"]["new_24h"], 1);
    let events: i64 = narrowed["errors"]["events_24h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_i64().unwrap())
        .sum();
    assert_eq!(events, 1);
    let names: Vec<&str> = narrowed["errors"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["app_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["iOS"]);
    assert_eq!(monitor_total(&narrowed), 0, "{narrowed}");

    let outsider = editor_session(&state).await.unwrap();
    let theirs = dashboard(String::new(), clone(&outsider)).await;
    assert_eq!(theirs["errors"]["apps"], 0);
    assert_eq!(theirs["errors"]["unresolved"], 0);
    assert_eq!(theirs["errors"]["issues"], json!([]));
    assert_eq!(monitor_total(&theirs), 0);
    let (status, _, _, _) = call(
        &app,
        "GET",
        &format!("/api/dashboard?project={mobile_project}"),
        None,
        Some(&outsider),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
