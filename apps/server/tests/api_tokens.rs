use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_server::{
    AppState, TestSession, boot, editor_session, member_session, router, testing_config,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const DAY_MS: i64 = 86_400_000;

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, String)],
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

fn cookie(session: &TestSession) -> Vec<(&'static str, String)> {
    vec![
        ("cookie", session.cookie.clone()),
        ("x-owlee-workspace", session.workspace_id.to_string()),
    ]
}

fn bearer(token: &str) -> Vec<(&'static str, String)> {
    vec![("authorization", format!("Bearer {token}"))]
}

async fn create_token(app: &Router, session: &TestSession, scopes: &[&str]) -> Value {
    let (status, created) = call(
        app,
        "POST",
        "/api/api-tokens",
        Some(json!({ "name": "CI", "scopes": scopes, "expires_in_days": null })),
        &cookie(session),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created
}

/// An error app with one ingested event, so the app has one issue. Returns the app and the issue.
async fn seed_issue(
    app: &Router,
    editor: &TestSession,
    project_id: &str,
    name: &str,
    event_id: &str,
) -> (Value, Value) {
    let (status, created) = call(
        app,
        "POST",
        "/api/error-apps",
        Some(json!({ "name": name, "platform": "php", "project_id": project_id })),
        &cookie(editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap();
    let key = created["publicKey"].as_str().unwrap();
    let event = json!({
        "event_id": event_id,
        "level": "error",
        "platform": "php",
        "environment": "production",
        "exception": { "values": [{ "type": "RuntimeException", "value": "disk full" }] },
    });
    let (status, body) = call(
        app,
        "POST",
        &format!("/api/{id}/store/"),
        Some(event),
        &[(
            "x-sentry-auth",
            format!("Sentry sentry_key={key}, sentry_version=7"),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, feed) = call(
        app,
        "GET",
        &format!("/api/error-issues?app_id={id}"),
        None,
        &cookie(editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    (created, feed["issues"][0].clone())
}

const EVENT_ONE: &str = "11111111111111111111111111111111";

async fn tokens_follow_their_owner(app: &Router, state: &AppState) {
    let editor = editor_session(state).await.unwrap();
    let project = editor.project_id.to_string();
    let (_, issue) = seed_issue(app, &editor, &project, "Checkout", EVENT_ONE).await;
    let issue_id = issue["id"].as_str().unwrap();

    let (status, created) = call(
        app,
        "POST",
        "/api/api-tokens",
        Some(json!({
            "name": "  Claude  ",
            "scopes": ["errors:read", "errors:read"],
            "expires_in_days": 30,
        })),
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let read = created["token"].as_str().unwrap().to_owned();
    assert!(read.starts_with("owl_"));
    assert_eq!(read.len(), 47);
    assert_eq!(created["prefix"], json!(&read[..12]));
    assert_eq!(created["name"], json!("Claude"));
    assert_eq!(created["scopes"], json!(["errors:read"]));
    assert_eq!(created["last_used_at"], Value::Null);
    let at = |field: &str| {
        chrono::DateTime::parse_from_rfc3339(created[field].as_str().unwrap())
            .unwrap()
            .timestamp_millis()
    };
    assert_eq!(at("expires_at") - at("created_at"), 30 * DAY_MS);

    let (status, list) = call(app, "GET", "/api/api-tokens", None, &cookie(&editor)).await;
    assert_eq!(status, StatusCode::OK);
    let mut listed = created.clone();
    listed.as_object_mut().unwrap().remove("token");
    assert_eq!(list, json!([listed]));

    let (status, feed) = call(app, "GET", "/api/error-issues", None, &bearer(&read)).await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    assert_eq!(feed["issues"][0]["id"], json!(issue_id));
    assert_eq!(feed["issues"][0]["title"], issue["title"]);
    let (_, list) = call(app, "GET", "/api/api-tokens", None, &cookie(&editor)).await;
    assert!(list[0]["last_used_at"].is_string(), "{list}");

    let (status, problem) = call(
        app,
        "PATCH",
        &format!("/api/error-issues/{issue_id}"),
        Some(json!({ "status": "resolved" })),
        &bearer(&read),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], json!("missing_scope"));
    assert_eq!(
        problem["detail"],
        json!("This token needs the errors:write scope.")
    );

    for (method, uri) in [
        ("GET", "/api/monitors"),
        ("GET", "/api/privacy/errors?user_id=9"),
        ("GET", "/api/api-tokens"),
        ("POST", "/api/api-tokens"),
        ("GET", "/api/auth/me"),
        ("GET", "/api/workspaces"),
    ] {
        let mut headers = bearer(&read);
        headers.extend(cookie(&editor));
        let (status, problem) = call(app, method, uri, None, &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {problem}");
        assert_eq!(problem["code"], json!("token_not_allowed"), "{uri}");
    }

    let other = editor_session(state).await.unwrap();
    let mut headers = bearer(&read);
    headers.push(("x-owlee-workspace", other.workspace_id.to_string()));
    let (status, problem) = call(app, "GET", "/api/error-issues", None, &headers).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["code"], json!("workspace_not_found"));
    let mut headers = bearer(&read);
    headers.push(("x-owlee-workspace", editor.workspace_id.to_string()));
    let (status, _) = call(app, "GET", "/api/error-issues", None, &headers).await;
    assert_eq!(status, StatusCode::OK);

    // A token is not a cookie, so the origin check does not apply.
    let write = create_token(app, &editor, &["errors:read", "errors:write"]).await;
    let write = write["token"].as_str().unwrap();
    let mut headers = bearer(write);
    headers.push(("origin", "https://elsewhere.example".to_owned()));
    let (status, updated) = call(
        app,
        "PATCH",
        &format!("/api/error-issues/{issue_id}"),
        Some(json!({ "status": "resolved" })),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["status"], json!("resolved"));

    let member = member_session(state, editor.workspace_id).await.unwrap();
    let (status, problem) = call(
        app,
        "POST",
        "/api/api-tokens",
        Some(json!({ "name": "Mine", "scopes": ["errors:read", "errors:write"], "expires_in_days": null })),
        &cookie(&member),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(
        problem["fields"],
        json!({ "scopes": "A workspace member can choose read scopes only." })
    );
    let member_token = create_token(app, &member, &["errors:read"]).await;
    let member_token = member_token["token"].as_str().unwrap().to_owned();
    let (status, feed) = call(
        app,
        "GET",
        "/api/error-issues",
        None,
        &bearer(&member_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(feed["issues"][0]["id"], json!(issue_id));

    let (status, problem) = call(
        app,
        "POST",
        "/api/api-tokens",
        Some(json!({ "name": " ", "scopes": ["root"], "expires_in_days": 14 })),
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        problem["fields"],
        json!({
            "name": "Use 1 to 100 characters.",
            "scopes": "Choose known scopes only.",
            "expires_in_days": "Choose 7, 30, 90, or 365 days, or no expiry.",
        })
    );
    let (status, problem) = call(
        app,
        "POST",
        "/api/api-tokens",
        Some(json!({ "name": "x".repeat(101), "scopes": [], "expires_in_days": null })),
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        problem["fields"],
        json!({ "name": "Use 1 to 100 characters.", "scopes": "Choose at least one scope." })
    );

    // Each user sees and revokes only their own tokens.
    let (_, member_list) = call(app, "GET", "/api/api-tokens", None, &cookie(&member)).await;
    assert_eq!(member_list.as_array().unwrap().len(), 1);
    let read_id = created["id"].as_str().unwrap();
    let (status, _) = call(
        app,
        "DELETE",
        &format!("/api/api-tokens/{read_id}"),
        None,
        &cookie(&member),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        app,
        "DELETE",
        &format!("/api/api-tokens/{read_id}"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, problem) = call(app, "GET", "/api/error-issues", None, &bearer(&read)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["code"], json!("unauthorized"));
    let (_, list) = call(app, "GET", "/api/api-tokens", None, &cookie(&editor)).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["scopes"], json!(["errors:read", "errors:write"]));

    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE workspace_id = $1 AND action IN ('api_token.created', 'api_token.revoked')",
    )
    .bind(editor.workspace_id.to_string())
    .fetch_one(state.db.pool())
    .await
    .unwrap();
    assert_eq!(audited, 4);

    sqlx::query("UPDATE api_tokens SET expires_at = 1 WHERE token_hash = $1")
        .bind(owlee_platform::hash_token(write))
        .execute(state.db.pool())
        .await
        .unwrap();
    let (status, _) = call(app, "GET", "/api/error-issues", None, &bearer(write)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    sqlx::query("UPDATE users SET disabled_at = 1 WHERE id = $1")
        .bind(member.user_id.to_string())
        .execute(state.db.pool())
        .await
        .unwrap();
    let (status, _) = call(
        app,
        "GET",
        "/api/error-issues",
        None,
        &bearer(&member_token),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = call(
        app,
        "GET",
        "/api/error-issues",
        None,
        &bearer("owl_unknown"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

async fn rpc(app: &Router, token: &str, message: Value) -> (StatusCode, Value) {
    call(app, "POST", "/api/mcp", Some(message), &bearer(token)).await
}

async fn tool(app: &Router, token: &str, name: &str, arguments: Value) -> Value {
    let (status, reply) = rpc(
        app,
        token,
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": arguments } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["id"], json!(7));
    reply["result"].clone()
}

async fn mcp_calls_the_errors_api(app: &Router, state: &AppState) {
    let editor = editor_session(state).await.unwrap();
    let project = editor.project_id.to_string();
    let (_, issue) = seed_issue(app, &editor, &project, "Checkout", EVENT_ONE).await;
    let read = create_token(app, &editor, &["errors:read"]).await;
    let read = read["token"].as_str().unwrap().to_owned();
    let write = create_token(app, &editor, &["errors:read", "errors:write"]).await;
    let write = write["token"].as_str().unwrap().to_owned();

    let (status, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["jsonrpc"], json!("2.0"));
    assert_eq!(reply["id"], json!(1));
    assert_eq!(reply["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(
        reply["result"]["capabilities"],
        json!({ "tools": { "listChanged": false } })
    );
    assert_eq!(
        reply["result"]["serverInfo"],
        json!({ "name": "owlee", "version": env!("CARGO_PKG_VERSION") })
    );
    let (_, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } }),
    )
    .await;
    assert_eq!(reply["result"]["protocolVersion"], json!("2025-11-25"));

    let (status, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!((status, reply), (StatusCode::ACCEPTED, Value::Null));
    let (_, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" }),
    )
    .await;
    assert_eq!(reply, json!({ "jsonrpc": "2.0", "id": "p", "result": {} }));
    let (_, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "server/discover" }),
    )
    .await;
    assert_eq!(reply["result"]["resultType"], json!("complete"));
    assert_eq!(
        reply["result"]["supportedVersions"],
        json!(["2025-11-25", "2025-06-18", "2025-03-26"])
    );
    assert_eq!(
        reply["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        json!("owlee")
    );

    let names = |reply: &Value| -> Vec<String> {
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let list = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" });
    let (_, reply) = rpc(app, &read, list.clone()).await;
    assert_eq!(
        names(&reply),
        [
            "list_error_apps",
            "list_issues",
            "get_issue",
            "list_issue_events",
            "get_event"
        ]
    );
    assert_eq!(
        reply["result"]["tools"][1]["inputSchema"]["properties"]["status"]["enum"],
        json!(["unresolved", "resolved", "ignored", "all"])
    );
    let (_, reply) = rpc(app, &write, list).await;
    assert_eq!(names(&reply).last().unwrap(), "update_issue_status");
    assert_eq!(
        reply["result"]["tools"][5]["inputSchema"]["required"],
        json!(["id", "status"])
    );

    let result = tool(app, &read, "list_issues", json!({ "status": "unresolved" })).await;
    assert_eq!(result["isError"], json!(false));
    assert_eq!(result["structuredContent"]["issues"][0]["id"], issue["id"]);
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);

    let short_id = issue["shortId"].as_str().unwrap();
    let result = tool(app, &read, "get_issue", json!({ "id": short_id })).await;
    assert_eq!(result["structuredContent"]["id"], issue["id"]);

    let result = tool(
        app,
        &read,
        "update_issue_status",
        json!({ "id": issue["id"], "status": "resolved" }),
    )
    .await;
    assert_eq!(
        result,
        json!({
            "content": [{ "type": "text", "text": "This token needs the errors:write scope." }],
            "isError": true,
        })
    );
    let result = tool(
        app,
        &write,
        "update_issue_status",
        json!({ "id": issue["id"], "status": "ignored" }),
    )
    .await;
    assert_eq!(result["structuredContent"]["status"], json!("ignored"));
    let result = tool(app, &read, "get_issue", json!({ "id": "../error-apps" })).await;
    assert_eq!(result["isError"], json!(true));

    let (_, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "drop_tables" } }),
    )
    .await;
    assert_eq!(reply["error"]["code"], json!(-32602));
    let (_, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": { "name": "get_issue", "arguments": {} } }),
    )
    .await;
    assert_eq!(reply["error"]["code"], json!(-32602));
    let (_, reply) = rpc(
        app,
        &read,
        json!({ "jsonrpc": "2.0", "id": 8, "method": "resources/list" }),
    )
    .await;
    assert_eq!(reply["error"]["code"], json!(-32601));
    let (status, reply) = rpc(
        app,
        &read,
        json!([{ "jsonrpc": "2.0", "id": 9, "method": "ping" }]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(reply["error"]["code"], json!(-32600));

    let ping = json!({ "jsonrpc": "2.0", "id": 10, "method": "ping" });
    let mut headers = bearer(&read);
    headers.push(("origin", "https://evil.example".to_owned()));
    let (status, _) = call(app, "POST", "/api/mcp", Some(ping.clone()), &headers).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut headers = bearer(&read);
    headers.push(("origin", "http://127.0.0.1".to_owned()));
    let (status, _) = call(app, "POST", "/api/mcp", Some(ping.clone()), &headers).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        app,
        "POST",
        "/api/mcp",
        Some(ping.clone()),
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(app, "POST", "/api/mcp", Some(ping), &bearer("owl_unknown")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for method in ["GET", "DELETE"] {
        let (status, _) = call(app, method, "/api/mcp", None, &bearer(&read)).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
}

async fn create_limited(
    app: &Router,
    session: &TestSession,
    scopes: &[&str],
    resources: Value,
) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/api/api-tokens",
        Some(json!({ "name": "Limited", "scopes": scopes, "expires_in_days": null, "resources": resources })),
        &cookie(session),
    )
    .await
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn resource_limits_hide_other_resources(app: &Router, state: &AppState) {
    let editor = editor_session(state).await.unwrap();
    let shop = editor.project_id.to_string();
    let (status, billing) = call(
        app,
        "POST",
        "/api/projects",
        Some(json!({ "name": "Billing" })),
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{billing}");
    let billing = billing["id"].as_str().unwrap().to_owned();
    let (checkout, shop_issue) = seed_issue(app, &editor, &shop, "Checkout", EVENT_ONE).await;
    let (invoices, billing_issue) = seed_issue(
        app,
        &editor,
        &billing,
        "Invoices",
        "22222222222222222222222222222222",
    )
    .await;
    let checkout = checkout["id"].as_str().unwrap().to_owned();
    let invoices = invoices["id"].as_str().unwrap().to_owned();
    let shop_issue_id = shop_issue["id"].as_str().unwrap().to_owned();
    let billing_issue_id = billing_issue["id"].as_str().unwrap().to_owned();
    let (_, detail) = call(
        app,
        "GET",
        &format!("/api/error-issues/{billing_issue_id}"),
        None,
        &cookie(&editor),
    )
    .await;
    let billing_event = detail["latestEventId"].as_str().unwrap().to_owned();
    let (_, detail) = call(
        app,
        "GET",
        &format!("/api/error-issues/{shop_issue_id}"),
        None,
        &cookie(&editor),
    )
    .await;
    let shop_event = detail["latestEventId"].as_str().unwrap().to_owned();

    let (status, by_project) = create_limited(
        app,
        &editor,
        &["errors:read", "errors:write", "errors:delete"],
        json!({ "projects": [shop, shop], "error_apps": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{by_project}");
    assert_eq!(
        by_project["resources"],
        json!({ "projects": [shop], "error_apps": [] })
    );
    let by_project = bearer(by_project["token"].as_str().unwrap());
    let (status, by_app) = create_limited(
        app,
        &editor,
        &["errors:read"],
        json!({ "projects": [], "error_apps": [checkout] }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{by_app}");
    let by_app_token = by_app["token"].as_str().unwrap().to_owned();
    let by_app = bearer(&by_app_token);
    let (_, list) = call(app, "GET", "/api/api-tokens", None, &cookie(&editor)).await;
    assert_eq!(
        list[0]["resources"],
        json!({ "projects": [], "error_apps": [checkout] })
    );
    assert_eq!(
        list[1]["resources"],
        json!({ "projects": [shop], "error_apps": [] })
    );

    let (status, apps) = call(app, "GET", "/api/error-apps", None, &by_project).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&apps), [checkout.clone()]);
    let (_, feed) = call(app, "GET", "/api/error-issues", None, &by_project).await;
    assert_eq!(ids(&feed["issues"]), [shop_issue_id.clone()]);
    assert_eq!(
        feed["counts"],
        json!({ "unresolved": 1, "resolved": 0, "ignored": 0 })
    );
    let (_, feed) = call(
        app,
        "GET",
        &format!("/api/error-issues?app_id={invoices}"),
        None,
        &by_project,
    )
    .await;
    assert_eq!(feed["issues"], json!([]));
    assert_eq!(feed["environments"], json!([]));
    let (status, _) = call(
        app,
        "GET",
        &format!("/api/error-events/{shop_event}"),
        None,
        &by_project,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let short_id = billing_issue["shortId"].as_str().unwrap();
    for (method, uri, body) in [
        ("GET", format!("/api/error-apps/{invoices}"), None),
        (
            "PUT",
            format!("/api/error-apps/{invoices}/quota"),
            Some(json!({ "daily_quota": 5 })),
        ),
        (
            "PUT",
            format!("/api/error-apps/{invoices}/protection"),
            Some(json!({ "allowedDomains": [], "inboundFilters": [], "spikeProtection": false })),
        ),
        ("DELETE", format!("/api/error-apps/{invoices}"), None),
        ("GET", format!("/api/error-issues/{billing_issue_id}"), None),
        ("GET", format!("/api/error-issues/{short_id}"), None),
        (
            "GET",
            format!("/api/error-issues/{billing_issue_id}/events"),
            None,
        ),
        (
            "GET",
            format!("/api/error-issues/{billing_issue_id}/events/{billing_event}/neighbors"),
            None,
        ),
        (
            "PATCH",
            format!("/api/error-issues/{billing_issue_id}"),
            Some(json!({ "status": "resolved" })),
        ),
        ("GET", format!("/api/error-events/{billing_event}"), None),
    ] {
        let (status, problem) = call(app, method, &uri, body, &by_project).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {problem}");
        assert_eq!(problem["code"], json!("not_found"), "{method} {uri}");
    }
    let (status, bulk) = call(
        app,
        "POST",
        "/api/error-issues/bulk",
        Some(json!({ "ids": [shop_issue_id, billing_issue_id, short_id], "status": "resolved" })),
        &by_project,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bulk}");
    assert_eq!(bulk, json!({ "updated": 1 }));
    let (_, untouched) = call(
        app,
        "GET",
        &format!("/api/error-issues/{billing_issue_id}"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(untouched["status"], json!("unresolved"));
    let (_, untouched) = call(
        app,
        "GET",
        &format!("/api/error-apps/{invoices}"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(untouched["quotaOverride"], Value::Null);
    assert_eq!(untouched["spikeProtection"], json!(true));

    let (status, problem) = call(
        app,
        "POST",
        "/api/error-apps",
        Some(json!({ "name": "Payroll", "platform": "php", "project_id": billing })),
        &by_project,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(problem["code"], json!("resource_not_allowed"));
    assert_eq!(
        problem["detail"],
        json!("This token cannot use this project.")
    );
    let (status, added) = call(
        app,
        "POST",
        "/api/error-apps",
        Some(json!({ "name": "Storefront", "platform": "php", "project_id": shop })),
        &by_project,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    let storefront = added["id"].as_str().unwrap().to_owned();

    let (_, apps) = call(app, "GET", "/api/error-apps", None, &by_project).await;
    assert_eq!(ids(&apps), [checkout.clone(), storefront.clone()]);
    let (_, apps) = call(app, "GET", "/api/error-apps", None, &by_app).await;
    assert_eq!(ids(&apps), [checkout.clone()]);
    let (status, _) = call(
        app,
        "GET",
        &format!("/api/error-apps/{storefront}"),
        None,
        &by_app,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, apps) = call(app, "GET", "/api/error-apps", None, &cookie(&editor)).await;
    assert_eq!(ids(&apps), [checkout.clone(), invoices.clone(), storefront]);

    let result = tool(
        app,
        &by_app_token,
        "list_issues",
        json!({ "status": "all" }),
    )
    .await;
    assert_eq!(
        ids(&result["structuredContent"]["issues"]),
        [shop_issue_id.clone()]
    );
    let result = tool(
        app,
        &by_app_token,
        "get_issue",
        json!({ "id": billing_issue_id }),
    )
    .await;
    assert_eq!(
        result,
        json!({
            "content": [{ "type": "text", "text": "That resource does not exist." }],
            "isError": true,
        })
    );

    let other = editor_session(state).await.unwrap();
    let (foreign_app, _) = seed_issue(
        app,
        &other,
        &other.project_id.to_string(),
        "Elsewhere",
        "33333333333333333333333333333333",
    )
    .await;
    for resources in [
        json!({ "projects": [], "error_apps": [] }),
        json!({ "projects": [other.project_id], "error_apps": [] }),
        json!({ "projects": [], "error_apps": [foreign_app["id"]] }),
        json!({ "projects": [], "error_apps": ["999999"] }),
        json!({ "projects": ["not-an-id"], "error_apps": [checkout] }),
    ] {
        let (status, problem) =
            create_limited(app, &editor, &["errors:read"], resources.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resources}");
        let message = if resources == json!({ "projects": [], "error_apps": [] }) {
            "Choose at least one project or app."
        } else {
            "Choose projects and apps in this workspace."
        };
        assert_eq!(
            problem["fields"],
            json!({ "resources": message }),
            "{resources}"
        );
    }
    let (status, unlimited) = create_limited(app, &editor, &["errors:read"], Value::Null).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(unlimited["resources"], Value::Null);
    let (_, apps) = call(
        app,
        "GET",
        "/api/error-apps",
        None,
        &bearer(unlimited["token"].as_str().unwrap()),
    )
    .await;
    assert_eq!(apps.as_array().unwrap().len(), 3);
}

async fn rotate_and_pause_keep_the_token(app: &Router, state: &AppState) {
    let editor = editor_session(state).await.unwrap();
    let member = member_session(state, editor.workspace_id).await.unwrap();
    let project = editor.project_id.to_string();
    let (error_app, _) = seed_issue(app, &editor, &project, "Billing", EVENT_ONE).await;
    let (status, created) = create_limited(
        app,
        &editor,
        &["errors:read"],
        json!({ "projects": [], "error_apps": [error_app["id"]] }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();
    let old = created["token"].as_str().unwrap();

    let (status, rotated) = call(
        app,
        "POST",
        &format!("/api/api-tokens/{id}/rotate"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let new = rotated["token"].as_str().unwrap();
    assert!(new.starts_with("owl_"));
    assert_eq!(rotated["prefix"].as_str().unwrap(), &new[..12]);
    assert_ne!(rotated["prefix"], created["prefix"]);
    for field in [
        "id",
        "name",
        "scopes",
        "resources",
        "created_at",
        "expires_at",
    ] {
        assert_eq!(rotated[field], created[field], "{field}");
    }
    assert_eq!(rotated["paused_at"], Value::Null);
    let (status, _) = call(app, "GET", "/api/error-issues", None, &bearer(old)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(app, "GET", "/api/error-issues", None, &bearer(new)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(app, "GET", "/api/api-tokens", None, &cookie(&editor)).await;
    assert!(list[0].get("token").is_none());
    assert_eq!(list[0]["prefix"], rotated["prefix"]);

    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let (status, paused) = call(
        app,
        "POST",
        &format!("/api/api-tokens/{id}/pause"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    let paused_at = paused["paused_at"].clone();
    assert!(paused_at.is_string(), "{paused}");
    let (status, again) = call(
        app,
        "POST",
        &format!("/api/api-tokens/{id}/pause"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["paused_at"], paused_at);
    let (_, list) = call(app, "GET", "/api/api-tokens", None, &cookie(&editor)).await;
    assert_eq!(list[0]["paused_at"], paused_at);
    let (status, _) = call(app, "GET", "/api/error-issues", None, &bearer(new)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = rpc(app, new, ping.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A paused token stays paused through a rotate.
    let (status, rotated) = call(
        app,
        "POST",
        &format!("/api/api-tokens/{id}/rotate"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rotated["paused_at"], paused_at);
    let new = rotated["token"].as_str().unwrap();
    let (status, _) = call(app, "GET", "/api/error-issues", None, &bearer(new)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, resumed) = call(
        app,
        "POST",
        &format!("/api/api-tokens/{id}/resume"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    assert_eq!(resumed["paused_at"], Value::Null);
    let (status, _) = call(app, "GET", "/api/error-issues", None, &bearer(new)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = rpc(app, new, ping).await;
    assert_eq!(status, StatusCode::OK);

    for action in ["rotate", "pause", "resume"] {
        let uri = format!("/api/api-tokens/{id}/{action}");
        let (status, _) = call(app, "POST", &uri, None, &cookie(&member)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{action}");
        let (status, problem) = call(app, "POST", &uri, None, &bearer(new)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{action}");
        assert_eq!(problem["code"], json!("token_not_allowed"), "{action}");
    }
    let (status, _) = call(app, "GET", "/api/error-issues", None, &bearer(new)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = call(
        app,
        "DELETE",
        &format!("/api/api-tokens/{id}"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        app,
        "POST",
        &format!("/api/api-tokens/{id}/rotate"),
        None,
        &cookie(&editor),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let mut audited: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE workspace_id = $1 AND resource_id = $2",
    )
    .bind(editor.workspace_id.to_string())
    .bind(id)
    .fetch_all(state.db.pool())
    .await
    .unwrap();
    audited.sort();
    assert_eq!(
        audited,
        [
            "api_token.created",
            "api_token.paused",
            "api_token.paused",
            "api_token.resumed",
            "api_token.revoked",
            "api_token.rotated",
            "api_token.rotated",
        ]
    );
}

async fn sqlite_app() -> (Router, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    let state = boot(testing_config(path)).await.unwrap();
    (router(state.clone()), state)
}

#[tokio::test]
async fn sqlite_tokens_follow_their_owner() {
    let (app, state) = sqlite_app().await;
    tokens_follow_their_owner(&app, &state).await;
}

#[tokio::test]
async fn sqlite_mcp_calls_the_errors_api() {
    let (app, state) = sqlite_app().await;
    mcp_calls_the_errors_api(&app, &state).await;
}

#[tokio::test]
async fn sqlite_resource_limits_hide_other_resources() {
    let (app, state) = sqlite_app().await;
    resource_limits_hide_other_resources(&app, &state).await;
}

#[tokio::test]
async fn sqlite_rotate_and_pause_keep_the_token() {
    let (app, state) = sqlite_app().await;
    rotate_and_pause_keep_the_token(&app, &state).await;
}

#[tokio::test]
async fn postgres_api_tokens_and_mcp() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| url.starts_with("postgres"))
    else {
        return;
    };
    let mut config = testing_config(std::path::PathBuf::from(
        "data/ignored-api-tokens-postgres.sqlite",
    ));
    config.database_url = Some(url);
    let state = boot(config).await.expect("postgres boot");
    let app = router(state.clone());
    tokens_follow_their_owner(&app, &state).await;
    mcp_calls_the_errors_api(&app, &state).await;
    resource_limits_hide_other_resources(&app, &state).await;
    rotate_and_pause_keep_the_token(&app, &state).await;
}
