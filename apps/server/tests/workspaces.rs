use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_platform::{Id, TimestampMillis, hash_token};
use owlee_server::{
    AppState, Outbox, TestSession, boot, editor_session, initialize_setup_token, member_session,
    permit_signup, router, testing_config,
};
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;

struct App {
    router: Router,
    state: AppState,
    outbox: Outbox,
}

async fn app() -> App {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    // Leak the temp dir so the sqlite file outlives the router.
    let _keep = Box::leak(Box::new(dir));
    started(testing_config(path)).await
}

async fn started(config: owlee_platform::Config) -> App {
    let mut state = boot(config).await.unwrap();
    let outbox = Outbox::default();
    state.outbox = Some(outbox.clone());
    App {
        router: router(state.clone()),
        state,
        outbox,
    }
}

async fn call(
    app: &App,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    workspace: Option<Id>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(workspace) = workspace {
        request = request.header("x-owlee-workspace", workspace.to_string());
    }
    let response = app
        .router
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// A call from `session` inside its own workspace.
async fn scoped(
    app: &App,
    session: &TestSession,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call(
        app,
        method,
        uri,
        body,
        Some(&session.cookie),
        Some(session.workspace_id),
    )
    .await
}

async fn email_of(app: &App, session: &TestSession) -> String {
    let (status, me) = call(
        app,
        "GET",
        "/api/auth/me",
        None,
        Some(&session.cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    me["email"].as_str().unwrap().to_owned()
}

fn token_of(invitation: &Value) -> String {
    let url = invitation["url"].as_str().expect("invitation url");
    let (origin, token) = url.split_once("/invite/").expect("invite path");
    assert_eq!(origin, "http://127.0.0.1");
    token.to_owned()
}

fn names(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn setup_gives_the_root_user_a_workspace_and_a_project() {
    let app = app().await;
    let token = initialize_setup_token(&app.state.db, TimestampMillis::now())
        .await
        .unwrap()
        .expect("fresh database issues one setup token");
    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup/complete")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "token": token.token,
                        "email": "ada@coollabs.io",
                        "password": "correct-horse-battery",
                        "display_name": "Ada"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let (status, me) = call(&app, "GET", "/api/auth/me", None, Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["root"], true);
    assert_eq!(me["installation_admin"], true);
    assert!(me.get("role").is_none(), "{me}");
    let workspaces = me["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1, "{me}");
    assert_eq!(workspaces[0]["name"], "My workspace");
    assert_eq!(workspaces[0]["role"], "owner");

    let workspace: Id = workspaces[0]["id"].as_str().unwrap().parse().unwrap();
    let (status, projects) = call(
        &app,
        "GET",
        "/api/projects",
        None,
        Some(&cookie),
        Some(workspace),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{projects}");
    assert_eq!(names(&projects), ["Default"]);
    assert_eq!(projects[0]["color"], "#4f46e5");
}

#[tokio::test]
async fn any_user_creates_a_workspace_with_a_default_project() {
    let app = app().await;
    let user = editor_session(&app.state).await.unwrap();

    let (status, problem) = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(json!({ "name": "   " })),
        Some(&user.cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["fields"]["name"], "Use 1 to 80 characters.");

    let (status, created) = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(json!({ "name": " Side project " })),
        Some(&user.cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "Side project");
    assert_eq!(created["role"], "owner");
    let created_id: Id = created["id"].as_str().unwrap().parse().unwrap();

    let (status, list) = call(
        &app,
        "GET",
        "/api/workspaces",
        None,
        Some(&user.cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(names(&list), ["Test workspace", "Side project"]);

    let (status, projects) = call(
        &app,
        "GET",
        "/api/projects",
        None,
        Some(&user.cookie),
        Some(created_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{projects}");
    assert_eq!(names(&projects), ["Default"]);

    let (status, renamed) = call(
        &app,
        "PUT",
        "/api/workspace",
        Some(json!({ "name": "Renamed" })),
        Some(&user.cookie),
        Some(created_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["name"], "Renamed");

    let (status, _) = call(
        &app,
        "DELETE",
        "/api/workspace",
        None,
        Some(&user.cookie),
        Some(created_id),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = call(
        &app,
        "GET",
        "/api/workspaces",
        None,
        Some(&user.cookie),
        None,
    )
    .await;
    assert_eq!(names(&list), ["Test workspace"]);
}

#[tokio::test]
async fn an_outsider_gets_404_for_a_foreign_workspace() {
    let app = app().await;
    let owner = editor_session(&app.state).await.unwrap();
    let outsider = editor_session(&app.state).await.unwrap();

    for workspace in [Some(owner.workspace_id), None] {
        let (status, problem) = call(
            &app,
            "GET",
            "/api/projects",
            None,
            Some(&outsider.cookie),
            workspace,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
        assert_eq!(problem["code"], "workspace_not_found");
    }

    let (status, problem) = call(
        &app,
        "GET",
        "/api/projects",
        None,
        None,
        Some(owner.workspace_id),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{problem}");

    // The outsider is an editor of their own workspace, and still cannot touch a foreign project.
    let uri = format!("/api/projects/{}", owner.project_id);
    let (status, problem) = scoped(
        &app,
        &outsider,
        "PUT",
        &uri,
        Some(json!({ "name": "Taken over", "color": "#000000" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    let (status, problem) = scoped(&app, &outsider, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    let (_, projects) = scoped(&app, &owner, "GET", "/api/projects", None).await;
    assert_eq!(names(&projects), ["Default"]);
}

#[tokio::test]
async fn a_member_reads_projects_and_cannot_change_them() {
    let app = app().await;
    let owner = editor_session(&app.state).await.unwrap();
    let member = member_session(&app.state, owner.workspace_id)
        .await
        .unwrap();
    assert_eq!(member.project_id, owner.project_id);

    let (status, projects) = scoped(&app, &member, "GET", "/api/projects", None).await;
    assert_eq!(status, StatusCode::OK, "{projects}");
    assert_eq!(names(&projects), ["Default"]);

    let (status, problem) = scoped(
        &app,
        &member,
        "POST",
        "/api/projects",
        Some(json!({ "name": "API", "color": "#0ea5e9" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let (status, problem) = scoped(&app, &member, "GET", "/api/invitations", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let (status, problem) = scoped(&app, &member, "DELETE", "/api/workspace", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
}

#[tokio::test]
async fn projects_keep_unique_names_and_the_last_one_stays() {
    let app = app().await;
    let owner = editor_session(&app.state).await.unwrap();

    let (status, problem) = scoped(
        &app,
        &owner,
        "DELETE",
        &format!("/api/projects/{}", owner.project_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");

    let (status, api) = scoped(
        &app,
        &owner,
        "POST",
        "/api/projects",
        Some(json!({ "name": "API", "color": "#0EA5E9" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{api}");
    assert_eq!(api["color"], "#0ea5e9");

    let (status, problem) = scoped(
        &app,
        &owner,
        "POST",
        "/api/projects",
        Some(json!({ "name": "API", "color": "#000000" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(
        problem["fields"]["name"],
        "A project with this name already exists."
    );
    let (status, problem) = scoped(
        &app,
        &owner,
        "POST",
        "/api/projects",
        Some(json!({ "name": "Web", "color": "blue" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["fields"]["color"], "Use a color like #4f46e5.");

    let api_uri = format!("/api/projects/{}", api["id"].as_str().unwrap());
    let (status, updated) = scoped(
        &app,
        &owner,
        "PUT",
        &api_uri,
        Some(json!({ "name": "Backend", "color": "#111111" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (_, projects) = scoped(&app, &owner, "GET", "/api/projects", None).await;
    assert_eq!(names(&projects), ["Default", "Backend"]);

    let (status, _) = scoped(&app, &owner, "DELETE", &api_uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, projects) = scoped(&app, &owner, "GET", "/api/projects", None).await;
    assert_eq!(names(&projects), ["Default"]);
}

#[tokio::test]
async fn an_invitation_is_accepted_only_by_the_invited_email() {
    let app = app().await;
    let owner = editor_session(&app.state).await.unwrap();
    let invitee = editor_session(&app.state).await.unwrap();
    let stranger = editor_session(&app.state).await.unwrap();
    let invitee_email = email_of(&app, &invitee).await;

    let (status, invitation) = scoped(
        &app,
        &owner,
        "POST",
        "/api/invitations",
        Some(json!({ "email": invitee_email, "role": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{invitation}");
    assert_eq!(invitation["email"], invitee_email);
    assert_eq!(invitation["role"], "admin");
    let token = token_of(&invitation);
    assert!(
        app.outbox.lock().unwrap().is_empty(),
        "no mail server is set"
    );

    let stored: String = sqlx::query("SELECT token_hash FROM workspace_invitations")
        .fetch_one(app.state.db.pool())
        .await
        .unwrap()
        .try_get("token_hash")
        .unwrap();
    assert_eq!(stored, hash_token(&token));

    let (status, pending) = scoped(&app, &owner, "GET", "/api/invitations", None).await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["email"], invitee_email);
    assert!(pending[0].get("url").is_none(), "{pending}");

    let (status, preview) = call(
        &app,
        "GET",
        &format!("/api/invitations/{token}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(
        preview,
        json!({ "workspace_name": "Test workspace", "email": invitee_email, "role": "admin" })
    );

    let accept = format!("/api/invitations/{token}/accept");
    let (status, problem) = call(&app, "POST", &accept, None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{problem}");
    let (status, problem) = call(&app, "POST", &accept, None, Some(&stranger.cookie), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let (status, problem) = call(
        &app,
        "GET",
        "/api/members",
        None,
        Some(&stranger.cookie),
        Some(owner.workspace_id),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");

    let (status, accepted) = call(&app, "POST", &accept, None, Some(&invitee.cookie), None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["workspace_id"], owner.workspace_id.to_string());

    let (status, members) = scoped(&app, &owner, "GET", "/api/members", None).await;
    assert_eq!(status, StatusCode::OK, "{members}");
    let roles: Vec<(&str, &str)> = members
        .as_array()
        .unwrap()
        .iter()
        .map(|member| {
            (
                member["email"].as_str().unwrap(),
                member["role"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        roles,
        [
            (email_of(&app, &owner).await.as_str(), "owner"),
            (invitee_email.as_str(), "admin")
        ]
    );

    let (status, problem) = call(&app, "POST", &accept, None, Some(&invitee.cookie), None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a link works once: {problem}"
    );
    let (_, pending) = scoped(&app, &owner, "GET", "/api/invitations", None).await;
    assert_eq!(pending, json!([]));

    let (status, problem) = scoped(
        &app,
        &owner,
        "POST",
        "/api/invitations",
        Some(json!({ "email": invitee_email, "role": "member" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(
        problem["fields"]["email"],
        "This person is already a member of the workspace."
    );
}

#[tokio::test]
async fn a_new_invitation_replaces_the_old_link_and_is_mailed() {
    let app = app().await;
    permit_signup(&app.state.db).await.unwrap();
    let owner = editor_session(&app.state).await.unwrap();
    let outsider = editor_session(&app.state).await.unwrap();
    let body = json!({ "email": "Grace@Example.com", "role": "member" });

    let (status, first) =
        scoped(&app, &owner, "POST", "/api/invitations", Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, second) = scoped(&app, &owner, "POST", "/api/invitations", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");

    let mails = app.outbox.lock().unwrap().clone();
    assert_eq!(mails.len(), 2);
    assert_eq!(mails[1].to, "grace@example.com");
    assert!(
        mails[1].text.contains(second["url"].as_str().unwrap()),
        "{}",
        mails[1].text
    );

    let first_link = format!("/api/invitations/{}", token_of(&first));
    let (status, problem) = call(&app, "GET", &first_link, None, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    let second_link = format!("/api/invitations/{}", token_of(&second));
    let (status, preview) = call(&app, "GET", &second_link, None, None, None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["email"], "Grace@Example.com");

    let by_id = format!("/api/invitations/{}", second["id"].as_str().unwrap());
    let (status, problem) = scoped(&app, &outsider, "DELETE", &by_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    let (status, _) = scoped(&app, &owner, "DELETE", &by_id, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, problem) = call(&app, "GET", &second_link, None, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
}

#[tokio::test]
async fn the_owner_stays_and_a_member_can_leave() {
    let app = app().await;
    let owner = editor_session(&app.state).await.unwrap();
    let member = member_session(&app.state, owner.workspace_id)
        .await
        .unwrap();
    let other = member_session(&app.state, owner.workspace_id)
        .await
        .unwrap();

    let (_, members) = scoped(&app, &owner, "GET", "/api/members", None).await;
    let membership_of = |user_id: Id| {
        let row = members
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["user_id"] == user_id.to_string())
            .expect("membership");
        format!("/api/members/{}", row["id"].as_str().unwrap())
    };
    let owner_uri = membership_of(owner.user_id);
    let member_uri = membership_of(member.user_id);
    let other_uri = membership_of(other.user_id);

    let (status, problem) = scoped(&app, &owner, "DELETE", &owner_uri, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    let (status, problem) = scoped(
        &app,
        &owner,
        "PUT",
        &owner_uri,
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");

    let (status, problem) = scoped(&app, &member, "DELETE", &other_uri, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let (status, problem) = scoped(
        &app,
        &member,
        "PUT",
        &member_uri,
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");

    let (status, promoted) = scoped(
        &app,
        &owner,
        "PUT",
        &other_uri,
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    assert_eq!(promoted["role"], "admin");
    let (status, problem) = scoped(&app, &other, "DELETE", &owner_uri, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");

    let (status, _) = scoped(&app, &member, "DELETE", &member_uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, problem) = scoped(&app, &member, "GET", "/api/projects", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    let (_, members) = scoped(&app, &owner, "GET", "/api/members", None).await;
    assert_eq!(members.as_array().unwrap().len(), 2);
}

/// Setup may already have run on a reused Postgres database, so this goes
/// through `editor_session` only.
#[tokio::test]
async fn removing_an_admin_revokes_their_pending_invitations() {
    let app = app().await;
    let owner = editor_session(&app.state).await.unwrap();
    let admin = member_session(&app.state, owner.workspace_id)
        .await
        .unwrap();
    let (_, members) = scoped(&app, &owner, "GET", "/api/members", None).await;
    let membership_id = members
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["user_id"] == admin.user_id.to_string())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let uri = format!("/api/members/{membership_id}");
    let (status, body) = scoped(&app, &owner, "PUT", &uri, Some(json!({"role": "admin"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let invite = json!({"email": "backdoor@example.com", "role": "admin"});
    let (status, body) = scoped(&app, &admin, "POST", "/api/invitations", Some(invite)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _) = scoped(&app, &owner, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, pending) = scoped(&app, &owner, "GET", "/api/invitations", None).await;
    assert_eq!(pending, json!([]));
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/invitations/{}", token_of(&body)),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn postgres_scopes_a_workspace_when_configured() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| !url.is_empty())
    else {
        return;
    };
    let mut config = testing_config(std::path::PathBuf::from(
        "data/ignored-workspaces-postgres.sqlite",
    ));
    config.database_url = Some(url);
    let app = started(config).await;
    let owner = editor_session(&app.state).await.unwrap();
    let member = member_session(&app.state, owner.workspace_id)
        .await
        .unwrap();
    let invitee = editor_session(&app.state).await.unwrap();

    let (status, problem) = scoped(
        &app,
        &owner,
        "DELETE",
        &format!("/api/projects/{}", owner.project_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    let project = json!({ "name": "API", "color": "#0ea5e9" });
    let (status, problem) = scoped(
        &app,
        &member,
        "POST",
        "/api/projects",
        Some(project.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let (status, api) = scoped(&app, &owner, "POST", "/api/projects", Some(project)).await;
    assert_eq!(status, StatusCode::CREATED, "{api}");
    let (status, renamed) = scoped(
        &app,
        &owner,
        "PUT",
        &format!("/api/projects/{}", api["id"].as_str().unwrap()),
        Some(json!({ "name": "Backend", "color": "#111111" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    let (status, projects) = scoped(&app, &member, "GET", "/api/projects", None).await;
    assert_eq!(status, StatusCode::OK, "{projects}");
    assert_eq!(names(&projects), ["Default", "Backend"]);

    let (status, problem) = call(
        &app,
        "GET",
        "/api/projects",
        None,
        Some(&invitee.cookie),
        Some(owner.workspace_id),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");

    let body = json!({ "email": email_of(&app, &invitee).await, "role": "member" });
    let (status, first) =
        scoped(&app, &owner, "POST", "/api/invitations", Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, second) = scoped(&app, &owner, "POST", "/api/invitations", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let (status, pending) = scoped(&app, &owner, "GET", "/api/invitations", None).await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(pending.as_array().unwrap().len(), 1);
    let accept = format!("/api/invitations/{}/accept", token_of(&second));
    let (status, accepted) = call(&app, "POST", &accept, None, Some(&invitee.cookie), None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    let (status, members) = scoped(&app, &owner, "GET", "/api/members", None).await;
    assert_eq!(status, StatusCode::OK, "{members}");
    assert_eq!(members.as_array().unwrap().len(), 3);
    let (_, me) = call(
        &app,
        "GET",
        "/api/auth/me",
        None,
        Some(&invitee.cookie),
        None,
    )
    .await;
    assert_eq!(me["workspaces"].as_array().unwrap().len(), 2, "{me}");

    let (status, _) = scoped(&app, &owner, "DELETE", "/api/workspace", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, me) = call(
        &app,
        "GET",
        "/api/auth/me",
        None,
        Some(&invitee.cookie),
        None,
    )
    .await;
    assert_eq!(me["workspaces"].as_array().unwrap().len(), 1, "{me}");
}
