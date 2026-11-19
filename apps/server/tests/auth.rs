//! Roles, email links, password changes, sessions, and the origin check.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_platform::{Id, TimestampMillis};
use owlee_server::{
    AppState, Outbox, boot, initialize_setup_token, member_session, permit_signup, router,
    testing_config,
};
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;

const PASSWORD: &str = "correct-horse-battery";
const ORIGIN: &str = "http://127.0.0.1";

struct Hit {
    status: StatusCode,
    body: Value,
    /// `name=value` of the session cookie the response set, if any.
    cookie: Option<String>,
    set_cookie: Vec<String>,
}

struct App {
    router: Router,
    state: AppState,
    outbox: Outbox,
    root: String,
    /// The workspace setup made for root, and its "Default" project.
    workspace: String,
    project: String,
}

impl App {
    fn scope(&self) -> [(&'static str, &str); 1] {
        [("x-owlee-workspace", self.workspace.as_str())]
    }

    /// A read-only member of root's workspace.
    async fn member(&self) -> String {
        let workspace = self.workspace.parse().unwrap();
        member_session(&self.state, workspace).await.unwrap().cookie
    }
}

async fn app() -> App {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.sqlite");
    let _keep = Box::leak(Box::new(dir));
    let mut state = boot(testing_config(path)).await.unwrap();
    let outbox = Outbox::default();
    state.outbox = Some(outbox.clone());
    let token = initialize_setup_token(&state.db, TimestampMillis::now())
        .await
        .unwrap()
        .expect("setup token")
        .token;
    let router = router(state.clone());
    let hit = call(
        &router,
        "POST",
        "/api/setup/complete",
        Some(json!({
            "token": token,
            "email": "root@coollabs.io",
            "password": PASSWORD,
            "display_name": "Root"
        })),
        None,
        &[],
    )
    .await;
    assert_eq!(hit.status, StatusCode::CREATED, "{}", hit.body);
    permit_signup(&state.db).await.unwrap();
    let root = hit.cookie.expect("root session");
    let workspace = hit.body["workspaces"][0]["id"]
        .as_str()
        .expect("setup workspace")
        .to_owned();
    let projects = call(
        &router,
        "GET",
        "/api/projects",
        None,
        Some(&root),
        &[("x-owlee-workspace", workspace.as_str())],
    )
    .await;
    let project = projects.body[0]["id"]
        .as_str()
        .expect("default project")
        .to_owned();
    App {
        router,
        state,
        outbox,
        root,
        workspace,
        project,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    headers: &[(&str, &str)],
) -> Hit {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    for (name, value) in headers {
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
    let set_cookie: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_owned)
        .collect();
    let cookie = set_cookie
        .iter()
        .filter_map(|value| value.split(';').next())
        .find(|pair| pair.starts_with("owlee_session=") && pair.len() > "owlee_session=".len())
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Hit {
        status,
        body,
        cookie,
        set_cookie,
    }
}

fn mails(outbox: &Outbox) -> Vec<owlee_server::SentMail> {
    outbox.lock().unwrap().clone()
}

fn link_token(text: &str) -> String {
    text.split("token=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("link in mail")
        .to_owned()
}

async fn register(app: &App, email: &str) -> (Value, String) {
    let asked = call(
        &app.router,
        "POST",
        "/api/auth/register",
        Some(json!({ "email": email })),
        None,
        &[],
    )
    .await;
    assert_eq!(asked.status, StatusCode::ACCEPTED, "{}", asked.body);
    let mail = mails(&app.outbox).pop().expect("register mail");
    let done = call(
        &app.router,
        "POST",
        "/api/auth/register/complete",
        Some(json!({
            "token": link_token(&mail.text),
            "password": PASSWORD,
            "display_name": "Member"
        })),
        None,
        &[],
    )
    .await;
    assert_eq!(done.status, StatusCode::CREATED, "{}", done.body);
    (done.body, done.cookie.expect("member session"))
}

async fn login(app: &App, email: &str, password: &str) -> Hit {
    call(
        &app.router,
        "POST",
        "/api/auth/login",
        Some(json!({ "email": email, "password": password })),
        None,
        &[],
    )
    .await
}

#[tokio::test]
async fn members_get_403_on_every_mutating_route_and_still_read() {
    let app = app().await;
    let member = app.member().await;
    let id = Id::new().to_string();
    let project = app.project.as_str();
    let monitor = json!({"project_id": project, "name": "API", "job_type": "http", "url": "https://example.com"});
    let page =
        json!({"project_id": project, "title": "Status", "slug": "status", "monitor_ids": []});
    let incident = json!({"project_id": project, "title": "Down", "status": "investigating", "impact": "minor", "message": "Looking"});
    let update = json!({"status": "resolved", "message": "Fixed"});
    let maintenance = json!({
        "project_id": project, "title": "Upgrade", "message": "Short", "page_id": id, "monitor_ids": [],
        "starts_at": "2026-12-01T00:00:00.000Z", "ends_at": "2026-12-01T01:00:00.000Z"
    });
    let channel = json!({"name": "Ops", "provider": "webhook", "config": {}, "monitor_ids": []});
    let public = json!({"enabled": true, "slug": "share", "threshold": 0, "sections": []});
    let domain = json!({"hostname": "go.example.com"});
    let link = json!({"project_id": project, "domain_id": id, "slug": "x", "destination": "https://example.com"});
    let quota = json!({"daily_quota": 10});
    let routes: Vec<(&str, String, Option<Value>)> = vec![
        ("POST", "/api/monitors".into(), Some(monitor.clone())),
        ("PUT", format!("/api/monitors/{id}"), Some(monitor)),
        ("DELETE", format!("/api/monitors/{id}"), None),
        ("POST", format!("/api/monitors/{id}/checks"), None),
        (
            "POST",
            format!("/api/monitors/{id}/active"),
            Some(json!({"active": false})),
        ),
        (
            "POST",
            "/api/agents".into(),
            Some(json!({"project_id": project, "name": "edge"})),
        ),
        ("DELETE", format!("/api/agents/{id}"), None),
        ("POST", "/api/pages".into(), Some(page.clone())),
        ("PUT", format!("/api/pages/{id}"), Some(page)),
        ("DELETE", format!("/api/pages/{id}"), None),
        ("POST", "/api/incidents".into(), Some(incident)),
        ("DELETE", format!("/api/incidents/{id}"), None),
        ("POST", format!("/api/incidents/{id}/updates"), Some(update)),
        ("POST", "/api/maintenances".into(), Some(maintenance)),
        ("DELETE", format!("/api/maintenances/{id}"), None),
        ("POST", "/api/notifications".into(), Some(channel.clone())),
        ("PUT", format!("/api/notifications/{id}"), Some(channel)),
        ("DELETE", format!("/api/notifications/{id}"), None),
        ("POST", format!("/api/notifications/{id}/test"), None),
        (
            "POST",
            "/api/sites".into(),
            Some(json!({"project_id": project, "name": "Site", "domain": "example.com"})),
        ),
        (
            "PUT",
            format!("/api/sites/{id}"),
            Some(json!({"name": "Site", "domain": "example.com"})),
        ),
        ("DELETE", format!("/api/sites/{id}"), None),
        ("POST", format!("/api/sites/{id}/verify"), None),
        ("PUT", format!("/api/sites/{id}/quota"), Some(quota.clone())),
        ("DELETE", format!("/api/sites/{id}/analytics"), None),
        (
            "PUT",
            format!("/api/sites/{id}/public"),
            Some(public.clone()),
        ),
        ("POST", "/api/domains".into(), Some(domain.clone())),
        ("PUT", format!("/api/domains/{id}"), Some(domain)),
        ("DELETE", format!("/api/domains/{id}"), None),
        ("POST", "/api/links".into(), Some(link.clone())),
        ("PUT", format!("/api/links/{id}"), Some(link)),
        ("DELETE", format!("/api/links/{id}"), None),
        ("PUT", format!("/api/links/{id}/public"), Some(public)),
        (
            "POST",
            "/api/error-apps".into(),
            Some(json!({"project_id": project, "name": "App", "platform": "php"})),
        ),
        ("DELETE", format!("/api/error-apps/{id}"), None),
        ("PUT", format!("/api/error-apps/{id}/quota"), Some(quota)),
        (
            "POST",
            "/api/error-issues/bulk".into(),
            Some(json!({"ids": [id], "status": "resolved"})),
        ),
        (
            "PATCH",
            format!("/api/error-issues/{id}"),
            Some(json!({"status": "resolved"})),
        ),
        (
            "PUT",
            "/api/admin/settings/registration".into(),
            Some(json!({"open": false})),
        ),
        (
            "PUT",
            "/api/admin/settings/quotas".into(),
            Some(json!({"error_daily": 1, "analytics_daily": 1})),
        ),
        (
            "PUT",
            "/api/admin/settings/smtp".into(),
            Some(json!({"host": "smtp.example.com", "from": "a@example.com"})),
        ),
        ("DELETE", "/api/admin/settings/smtp".into(), None),
        (
            "PUT",
            format!("/api/admin/users/{id}/admin"),
            Some(json!({"admin": true})),
        ),
        ("GET", "/api/notifications".into(), None),
        ("GET", "/api/admin/users".into(), None),
    ];
    let mut allowed = Vec::new();
    for (method, path, body) in &routes {
        let hit = call(
            &app.router,
            method,
            path,
            body.clone(),
            Some(&member),
            &app.scope(),
        )
        .await;
        if hit.status != StatusCode::FORBIDDEN {
            allowed.push(format!("{method} {path} -> {} {}", hit.status, hit.body));
        }
    }
    assert!(
        allowed.is_empty(),
        "members reached:\n{}",
        allowed.join("\n")
    );

    let agent = json!({"project_id": project, "name": "edge"});
    let denied = call(
        &app.router,
        "POST",
        "/api/agents",
        Some(agent.clone()),
        Some(&member),
        &app.scope(),
    )
    .await;
    assert_eq!(
        denied.body["detail"],
        "Only a workspace admin can change this."
    );
    for path in [
        "/api/monitors",
        "/api/overview",
        "/api/agents",
        "/api/pages",
        "/api/incidents",
        "/api/maintenances",
        "/api/sites",
        "/api/error-apps",
        "/api/auth/sessions",
    ] {
        let hit = call(&app.router, "GET", path, None, Some(&member), &app.scope()).await;
        assert_eq!(hit.status, StatusCode::OK, "{path} {}", hit.body);
    }
    let created = call(
        &app.router,
        "POST",
        "/api/agents",
        Some(agent),
        Some(&app.root),
        &app.scope(),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
}

#[tokio::test]
async fn members_read_monitors_without_request_headers_or_body() {
    let app = app().await;
    let member = app.member().await;
    let created = call(
        &app.router,
        "POST",
        "/api/monitors",
        Some(json!({
            "project_id": app.project,
            "name": "API", "job_type": "http", "url": "https://ops:hunter2@example.com/health?api_key=abc", "method": "POST",
            "headers": [{"name": "Authorization", "value": "Bearer secret"}],
            "body": "{\"token\":\"secret\"}"
        })),
        Some(&app.root),
        &app.scope(),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().expect("monitor id").to_owned();
    let detail = format!("/api/monitors/{id}");

    let admin = call(
        &app.router,
        "GET",
        &detail,
        None,
        Some(&app.root),
        &app.scope(),
    )
    .await;
    assert_eq!(
        admin.body["headers"],
        json!([{"name": "Authorization", "value": "Bearer secret"}])
    );
    assert_eq!(admin.body["body"], "{\"token\":\"secret\"}");
    assert_eq!(
        admin.body["url"],
        "https://ops:hunter2@example.com/health?api_key=abc"
    );

    let seen = call(
        &app.router,
        "GET",
        &detail,
        None,
        Some(&member),
        &app.scope(),
    )
    .await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert_eq!(seen.body["headers"], json!([]));
    assert_eq!(seen.body["body"], "");
    assert_eq!(seen.body["url"], "https://example.com/health");
    let list = call(
        &app.router,
        "GET",
        "/api/monitors",
        None,
        Some(&member),
        &app.scope(),
    )
    .await;
    assert_eq!(list.body[0]["headers"], json!([]));
    assert_eq!(list.body[0]["body"], "");
    assert_eq!(list.body[0]["url"], "https://example.com/health");
    let overview = call(
        &app.router,
        "GET",
        "/api/overview",
        None,
        Some(&member),
        &app.scope(),
    )
    .await;
    assert_eq!(overview.body["monitors"][0]["headers"], json!([]));
    assert_eq!(overview.body["monitors"][0]["body"], "");
}

#[tokio::test]
async fn register_sends_a_single_use_link_and_answers_the_same_for_taken_emails() {
    let app = app().await;
    let asked = call(
        &app.router,
        "POST",
        "/api/auth/register",
        Some(json!({"email": " New@Coollabs.io "})),
        None,
        &[],
    )
    .await;
    assert_eq!(asked.status, StatusCode::ACCEPTED);
    assert_eq!(asked.body, json!({}));
    assert!(asked.cookie.is_none());
    let mail = mails(&app.outbox).pop().unwrap();
    assert_eq!(mail.to, "new@coollabs.io");
    assert_eq!(mail.subject, "Finish creating your Owlee account");
    assert!(
        mail.text
            .contains("http://127.0.0.1/register/complete?token="),
        "{}",
        mail.text
    );
    let token = link_token(&mail.text);

    let stored: i64 = sqlx::query("SELECT COUNT(*) AS n FROM email_tokens WHERE token_hash = $1")
        .bind(&token)
        .fetch_one(app.state.db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(stored, 0, "the raw token is not stored");

    let weak = call(
        &app.router,
        "POST",
        "/api/auth/register/complete",
        Some(json!({"token": token, "password": "short"})),
        None,
        &[],
    )
    .await;
    assert_eq!(weak.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(weak.body["fields"]["password"].is_string(), "{}", weak.body);

    let done = call(
        &app.router,
        "POST",
        "/api/auth/register/complete",
        Some(json!({"token": token, "password": PASSWORD})),
        None,
        &[],
    )
    .await;
    assert_eq!(done.status, StatusCode::CREATED, "{}", done.body);
    assert_eq!(done.body["email"], "new@coollabs.io");
    assert_eq!(done.body["display_name"], "new");
    assert_eq!(
        done.body["workspaces"],
        json!([]),
        "register makes no workspace"
    );
    assert_eq!(done.body["installation_admin"], false);
    let session = done
        .set_cookie
        .iter()
        .find(|value| value.starts_with("owlee_session="))
        .unwrap();
    assert!(session.contains("Max-Age=7776000"), "{session}");

    let again = call(
        &app.router,
        "POST",
        "/api/auth/register/complete",
        Some(json!({"token": token, "password": PASSWORD})),
        None,
        &[],
    )
    .await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        again.body["fields"]["token"],
        "This link is invalid or has expired. Ask for a new one."
    );

    let taken = call(
        &app.router,
        "POST",
        "/api/auth/register",
        Some(json!({"email": "new@coollabs.io"})),
        None,
        &[],
    )
    .await;
    assert_eq!(taken.status, asked.status);
    assert_eq!(taken.body, asked.body);
    let mail = mails(&app.outbox).pop().unwrap();
    assert_eq!(mail.subject, "You already have an Owlee account");
    assert!(!mail.text.contains("token="), "{}", mail.text);
    assert!(
        mail.text.contains("http://127.0.0.1/forgot-password"),
        "{}",
        mail.text
    );

    let old_shape = call(
        &app.router,
        "POST",
        "/api/auth/register",
        Some(json!({"email": "x@coollabs.io", "password": PASSWORD})),
        None,
        &[],
    )
    .await;
    assert_eq!(old_shape.status, StatusCode::ACCEPTED);
    assert!(old_shape.cookie.is_none(), "one-step registration is gone");
}

#[tokio::test]
async fn register_mails_stop_after_three_an_hour_per_address() {
    let app = app().await;
    for _ in 0..4 {
        let hit = call(
            &app.router,
            "POST",
            "/api/auth/register",
            Some(json!({"email": "flood@coollabs.io"})),
            None,
            &[],
        )
        .await;
        assert_eq!(hit.status, StatusCode::ACCEPTED);
    }
    assert_eq!(mails(&app.outbox).len(), 3);
}

#[tokio::test]
async fn recovery_sets_the_password_and_signs_out_every_session() {
    let app = app().await;
    let (_, first) = register(&app, "member@coollabs.io").await;
    let second = login(&app, "member@coollabs.io", PASSWORD)
        .await
        .cookie
        .unwrap();

    let unknown = call(
        &app.router,
        "POST",
        "/api/auth/recovery",
        Some(json!({"email": "nobody@coollabs.io"})),
        None,
        &[],
    )
    .await;
    assert_eq!(unknown.status, StatusCode::ACCEPTED);
    assert_eq!(unknown.body, json!({}));
    let before = mails(&app.outbox).len();
    let known = call(
        &app.router,
        "POST",
        "/api/auth/recovery",
        Some(json!({"email": "member@coollabs.io"})),
        None,
        &[],
    )
    .await;
    assert_eq!(known.status, unknown.status);
    assert_eq!(known.body, unknown.body);
    let sent = mails(&app.outbox);
    assert_eq!(sent.len(), before + 1, "only the known address gets mail");
    let mail = sent.last().unwrap();
    assert_eq!(mail.subject, "Reset your Owlee password");
    assert!(
        mail.text.contains("http://127.0.0.1/reset-password?token="),
        "{}",
        mail.text
    );

    let reset = call(
        &app.router,
        "POST",
        "/api/auth/recovery/complete",
        Some(json!({"token": link_token(&mail.text), "password": "a-brand-new-passphrase"})),
        None,
        &[],
    )
    .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    assert_eq!(reset.body["email"], "member@coollabs.io");
    let fresh = reset.cookie.expect("new session");

    for old in [&first, &second] {
        let hit = call(&app.router, "GET", "/api/auth/me", None, Some(old), &[]).await;
        assert_eq!(hit.status, StatusCode::UNAUTHORIZED);
    }
    let me = call(&app.router, "GET", "/api/auth/me", None, Some(&fresh), &[]).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(
        login(&app, "member@coollabs.io", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "member@coollabs.io", "a-brand-new-passphrase")
            .await
            .status,
        StatusCode::OK
    );

    let reused = call(
        &app.router,
        "POST",
        "/api/auth/recovery/complete",
        Some(json!({"token": link_token(&mail.text), "password": "another-new-passphrase"})),
        None,
        &[],
    )
    .await;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reused.body["fields"]["token"].is_string());
}

#[tokio::test]
async fn changing_the_password_keeps_this_session_and_ends_the_others() {
    let app = app().await;
    let (_, current) = register(&app, "member@coollabs.io").await;
    let other = login(&app, "member@coollabs.io", PASSWORD)
        .await
        .cookie
        .unwrap();

    let wrong = call(
        &app.router,
        "POST",
        "/api/auth/password",
        Some(json!({"current_password": "not-my-password", "new_password": "a-brand-new-passphrase"})),
        Some(&current),
        &[],
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        wrong.body["fields"]["current_password"],
        "The current password is wrong."
    );

    let common = call(
        &app.router,
        "POST",
        "/api/auth/password",
        Some(json!({"current_password": PASSWORD, "new_password": "ChangeMe1234"})),
        Some(&current),
        &[],
    )
    .await;
    assert_eq!(common.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        common.body["fields"]["new_password"],
        "password is too common"
    );

    let changed = call(
        &app.router,
        "POST",
        "/api/auth/password",
        Some(json!({"current_password": PASSWORD, "new_password": "a-brand-new-passphrase"})),
        Some(&current),
        &[],
    )
    .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT, "{}", changed.body);
    assert_eq!(
        call(
            &app.router,
            "GET",
            "/api/auth/me",
            None,
            Some(&current),
            &[]
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        call(&app.router, "GET", "/api/auth/me", None, Some(&other), &[])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "member@coollabs.io", "a-brand-new-passphrase")
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn sessions_list_and_revoke_only_the_callers_own() {
    let app = app().await;
    let (_, member) = register(&app, "member@coollabs.io").await;
    let other = login(&app, "member@coollabs.io", PASSWORD)
        .await
        .cookie
        .unwrap();

    let listed = call(
        &app.router,
        "GET",
        "/api/auth/sessions",
        None,
        Some(&member),
        &[],
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK);
    let sessions = listed.body.as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions
            .iter()
            .filter(|session| session["current"] == true)
            .count(),
        1
    );
    let current_id = sessions.iter().find(|s| s["current"] == true).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_id = sessions.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let root_sessions = call(
        &app.router,
        "GET",
        "/api/auth/sessions",
        None,
        Some(&app.root),
        &[],
    )
    .await;
    let root_id = root_sessions.body[0]["id"].as_str().unwrap().to_owned();
    let stolen = call(
        &app.router,
        "DELETE",
        &format!("/api/auth/sessions/{root_id}"),
        None,
        Some(&member),
        &[],
    )
    .await;
    assert_eq!(stolen.status, StatusCode::NOT_FOUND);
    assert_eq!(
        call(
            &app.router,
            "GET",
            "/api/auth/me",
            None,
            Some(&app.root),
            &[]
        )
        .await
        .status,
        StatusCode::OK
    );

    let revoked = call(
        &app.router,
        "DELETE",
        &format!("/api/auth/sessions/{other_id}"),
        None,
        Some(&member),
        &[],
    )
    .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    assert!(revoked.set_cookie.is_empty(), "{:?}", revoked.set_cookie);
    assert_eq!(
        call(&app.router, "GET", "/api/auth/me", None, Some(&other), &[])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    let own = call(
        &app.router,
        "DELETE",
        &format!("/api/auth/sessions/{current_id}"),
        None,
        Some(&member),
        &[],
    )
    .await;
    assert_eq!(own.status, StatusCode::NO_CONTENT);
    assert!(
        own.set_cookie
            .iter()
            .any(|value| value.starts_with("owlee_session=;")),
        "{:?}",
        own.set_cookie
    );
    assert_eq!(
        call(&app.router, "GET", "/api/auth/me", None, Some(&member), &[])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn cookie_routes_refuse_other_origins_and_ingest_does_not() {
    let app = app().await;
    let agent = Some(json!({"project_id": app.project, "name": "edge"}));
    let scope = app.scope()[0];
    let foreign = call(
        &app.router,
        "POST",
        "/api/agents",
        agent.clone(),
        Some(&app.root),
        &[scope, ("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    assert_eq!(
        foreign.body["detail"],
        "This request came from another site."
    );
    let cross = call(
        &app.router,
        "POST",
        "/api/agents",
        agent.clone(),
        Some(&app.root),
        &[scope, ("sec-fetch-site", "cross-site")],
    )
    .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    let login_elsewhere = call(
        &app.router,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "root@coollabs.io", "password": PASSWORD})),
        None,
        &[("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(login_elsewhere.status, StatusCode::FORBIDDEN);
    let same = call(
        &app.router,
        "POST",
        "/api/agents",
        agent,
        Some(&app.root),
        &[scope, ("origin", ORIGIN), ("sec-fetch-site", "same-origin")],
    )
    .await;
    assert_eq!(same.status, StatusCode::CREATED, "{}", same.body);
    let read = call(
        &app.router,
        "GET",
        "/api/agents",
        None,
        Some(&app.root),
        &[scope, ("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(read.status, StatusCode::OK);

    let project = call(
        &app.router,
        "POST",
        "/api/error-apps",
        Some(json!({"project_id": app.project, "name": "App", "platform": "php"})),
        Some(&app.root),
        &[scope],
    )
    .await;
    assert_eq!(project.status, StatusCode::OK, "{}", project.body);
    let id = project.body["id"].as_str().unwrap();
    let key = project.body["publicKey"].as_str().unwrap();
    let auth = format!("Sentry sentry_key={key}, sentry_version=7");
    let ingest = call(
        &app.router,
        "POST",
        &format!("/api/{id}/store/"),
        Some(json!({"event_id": "0123456789abcdef0123456789abcdef", "message": "boom", "platform": "php"})),
        None,
        &[("origin", "https://shop.example"), ("sec-fetch-site", "cross-site"), ("authorization", &auth)],
    )
    .await;
    assert_eq!(ingest.status, StatusCode::OK, "{}", ingest.body);
}

#[tokio::test]
async fn smtp_password_is_sealed_in_the_database() {
    let app = app().await;
    let saved = call(
        &app.router,
        "PUT",
        "/api/admin/settings/smtp",
        Some(json!({"host": "smtp.example.com", "password": "secret-pass", "from": "status@example.com"})),
        Some(&app.root),
        &[],
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["smtp"]["has_password"], true);
    let stored: String = sqlx::query("SELECT smtp_password FROM instance_settings WHERE id = 1")
        .fetch_one(app.state.db.pool())
        .await
        .unwrap()
        .try_get("smtp_password")
        .unwrap();
    assert!(stored.starts_with("v1:"), "{stored}");
    assert!(!stored.contains("secret-pass"));

    sqlx::query("UPDATE instance_settings SET smtp_password = 'old-plaintext' WHERE id = 1")
        .execute(app.state.db.pool())
        .await
        .unwrap();
    let settings = call(
        &app.router,
        "GET",
        "/api/admin/settings",
        None,
        Some(&app.root),
        &[],
    )
    .await;
    assert_eq!(
        settings.body["smtp"]["has_password"], false,
        "plaintext is not used"
    );
}
