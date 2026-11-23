use std::collections::BTreeMap;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use owlee_server::{TestSession, boot, editor_session, router, testing_config};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

const EVENT_ID: &str = "22222222222222222222222222222222";

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    headers: &[(&str, String)],
) -> (StatusCode, HeaderMap, Value) {
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
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("{method} {uri}: {}", String::from_utf8_lossy(&bytes)))
    };
    (status, headers, json)
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

/// Rewrites `#/components/schemas/X` to `#/$defs/X` and closes every object
/// schema with `unevaluatedProperties: false`, so a field the server sends but
/// the document lacks fails like a missing one. A member of `allOf` stays open
/// at its top (`X__open`), because its parent sees the merged properties.
fn strict(schema: &Value, open_top: bool) -> Value {
    match schema {
        Value::Object(object) => {
            let mut out = Map::new();
            for (key, value) in object {
                let value = match (key.as_str(), value) {
                    ("$ref", Value::String(target)) => {
                        let name = target
                            .strip_prefix("#/components/schemas/")
                            .unwrap_or_else(|| panic!("unexpected $ref {target}"));
                        let suffix = if open_top { "__open" } else { "" };
                        Value::String(format!("#/$defs/{name}{suffix}"))
                    }
                    ("allOf", Value::Array(members)) => {
                        Value::Array(members.iter().map(|member| strict(member, true)).collect())
                    }
                    _ => strict(value, false),
                };
                out.insert(key.clone(), value);
            }
            let is_object_schema =
                object.contains_key("properties") || object.contains_key("allOf");
            if is_object_schema && !open_top {
                out.insert("unevaluatedProperties".into(), Value::Bool(false));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| strict(item, false)).collect()),
        other => other.clone(),
    }
}

struct Contract {
    defs: Map<String, Value>,
}

impl Contract {
    fn new(document: &Value) -> Self {
        let mut defs = Map::new();
        for (name, schema) in document["components"]["schemas"].as_object().unwrap() {
            defs.insert(name.clone(), strict(schema, false));
            defs.insert(format!("{name}__open"), strict(schema, true));
        }
        Self { defs }
    }

    /// Every violation of `schema` by `instance`, one line each.
    fn violations(&self, schema: &Value, instance: &Value) -> Vec<String> {
        let mut root = strict(schema, false);
        root["$defs"] = Value::Object(self.defs.clone());
        let validator = jsonschema::draft202012::options()
            .should_validate_formats(true)
            .build(&root)
            .expect("the schema compiles");
        validator
            .iter_errors(instance)
            .map(|error| format!("{}: {error}", error.instance_path()))
            .collect()
    }
}

fn resolve<'a>(document: &'a Value, item: &'a Value) -> &'a Value {
    match item["$ref"].as_str() {
        Some(target) => document
            .pointer(target.trim_start_matches('#'))
            .unwrap_or_else(|| panic!("dangling $ref {target}")),
        None => item,
    }
}

struct Operation {
    id: String,
    method: String,
    path: String,
    parameters: Vec<Value>,
    request_schema: Option<Value>,
    success: (StatusCode, Value),
}

fn operations(document: &Value) -> Vec<Operation> {
    let mut found = Vec::new();
    for (path, item) in document["paths"].as_object().unwrap() {
        let shared = item["parameters"].as_array().cloned().unwrap_or_default();
        for (method, operation) in item.as_object().unwrap() {
            if method == "parameters" {
                continue;
            }
            let mut parameters: Vec<Value> = shared
                .iter()
                .chain(operation["parameters"].as_array().into_iter().flatten())
                .map(|parameter| resolve(document, parameter).clone())
                .collect();
            parameters.dedup_by(|a, b| a["name"] == b["name"] && a["in"] == b["in"]);
            let request_schema = operation
                .pointer("/requestBody/content/application~1json/schema")
                .cloned();
            let responses = operation["responses"].as_object().unwrap();
            let (code, response) = responses
                .iter()
                .find(|(code, _)| code.starts_with('2'))
                .unwrap_or_else(|| panic!("{method} {path} documents no 2xx"));
            let schema = resolve(document, response)
                .pointer("/content/application~1json/schema")
                .unwrap_or_else(|| panic!("{method} {path} {code} has no JSON schema"))
                .clone();
            found.push(Operation {
                id: operation["operationId"].as_str().unwrap().to_owned(),
                method: method.to_uppercase(),
                path: path.clone(),
                parameters,
                request_schema,
                success: (code.parse().unwrap(), schema),
            });
        }
    }
    found
}

/// Reads first and deletes last, so every operation finds the seeded rows.
fn order(method: &str) -> u8 {
    match method {
        "GET" | "HEAD" => 0,
        "DELETE" => 2,
        _ => 1,
    }
}

struct Seed {
    project_id: String,
    app_id: String,
    issue_id: String,
    event_id: String,
}

impl Seed {
    /// Ids by the resource the path names, so a route for a new resource fails here until it is seeded.
    fn path_value(&self, path: &str, name: &str) -> &str {
        match (path.split('/').nth(1), name) {
            (_, "event_id") | (Some("error-events"), "id") => &self.event_id,
            (Some("error-apps"), "id") => &self.app_id,
            (Some("error-issues"), "id") => &self.issue_id,
            _ => panic!("no seeded value for {{{name}}} in {path}"),
        }
    }

    fn body(&self, operation_id: &str) -> Value {
        match operation_id {
            "create_error_app" => {
                json!({ "name": "Billing", "platform": "node", "project_id": self.project_id })
            }
            "set_error_app_quota" => json!({ "daily_quota": 5000 }),
            "set_error_app_protection" => json!({
                "allowedDomains": ["https://shop.example.com/cart"],
                "inboundFilters": ["localhost"],
                "spikeProtection": true,
            }),
            "bulk_update_issue_status" => json!({ "ids": [self.issue_id], "status": "ignored" }),
            "update_issue_status" => json!({ "status": "resolved" }),
            other => panic!("no request body for {other}"),
        }
    }
}

async fn seed(app: &Router, editor: &TestSession) -> Seed {
    let project_id = editor.project_id.to_string();
    let (status, _, created) = send(
        app,
        "POST",
        "/api/error-apps",
        Some(&json!({ "name": "Checkout", "platform": "javascript", "project_id": project_id })),
        &cookie(editor),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let app_id = created["id"].as_str().unwrap().to_owned();
    let key = created["publicKey"].as_str().unwrap();
    let event = json!({
        "event_id": EVENT_ID,
        "level": "error",
        "platform": "javascript",
        "environment": "production",
        "release": "shop@1.4.0",
        "server_name": "web-1",
        "logger": "app",
        "sdk": { "name": "sentry.javascript.browser", "version": "8.0.0" },
        "user": { "email": "ada@example.com" },
        "tags": { "browser": "Firefox" },
        "contexts": { "os": { "name": "Linux" } },
        "breadcrumbs": { "values": [
            { "timestamp": 1_700_000_000.0, "category": "ui.click", "message": "Pay", "level": "info" }
        ] },
        "exception": { "values": [{
            "type": "TypeError",
            "value": "cart is undefined",
            "mechanism": { "type": "onerror", "handled": false },
            "stacktrace": { "frames": [{
                "filename": "app.js",
                "function": "checkout",
                "lineno": 12,
                "colno": 4,
                "in_app": true,
                "context_line": "cart.total()",
                "pre_context": ["function checkout() {"],
                "post_context": ["}"],
            }] },
        }] },
    });
    let (status, _, body) = send(
        app,
        "POST",
        &format!("/api/{app_id}/store/"),
        Some(&event),
        &[(
            "x-sentry-auth",
            format!("Sentry sentry_key={key}, sentry_version=7"),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, _, feed) = send(
        app,
        "GET",
        &format!("/api/error-issues?app_id={app_id}"),
        None,
        &cookie(editor),
    )
    .await;
    let issue_id = feed["issues"][0]["id"].as_str().unwrap().to_owned();
    let (_, _, events) = send(
        app,
        "GET",
        &format!("/api/error-issues/{issue_id}/events"),
        None,
        &cookie(editor),
    )
    .await;
    let event_id = events["items"][0]["id"].as_str().unwrap().to_owned();
    Seed {
        project_id,
        app_id,
        issue_id,
        event_id,
    }
}

async fn all_scopes_token(app: &Router, editor: &TestSession) -> String {
    let (status, _, created) = send(
        app,
        "POST",
        "/api/api-tokens",
        Some(&json!({
            "name": "Contract",
            "scopes": ["errors:read", "errors:write", "errors:delete"],
            "expires_in_days": null,
        })),
        &cookie(editor),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["token"].as_str().unwrap().to_owned()
}

async fn sqlite_app() -> (Router, owlee_server::AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    let state = boot(testing_config(path)).await.unwrap();
    (router(state.clone()), state)
}

async fn served_document(app: &Router) -> Value {
    let (status, _, document) = send(app, "GET", "/api/v1/openapi.json", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    document
}

#[tokio::test]
async fn every_documented_operation_matches_the_router() {
    let (app, state) = sqlite_app().await;
    let editor = editor_session(&state).await.unwrap();
    let seed = seed(&app, &editor).await;
    let token = all_scopes_token(&app, &editor).await;
    let document = served_document(&app).await;
    let contract = Contract::new(&document);

    let mut operations = operations(&document);
    operations.sort_by_key(|operation| order(&operation.method));
    assert_eq!(
        operations.len(),
        13,
        "the walk found every operation in the document"
    );

    let mut failures = BTreeMap::new();
    for operation in &operations {
        let mut uri = format!("/api{}", operation.path);
        for parameter in &operation.parameters {
            let name = parameter["name"].as_str().unwrap();
            match parameter["in"].as_str().unwrap() {
                "path" => {
                    uri = uri.replace(
                        &format!("{{{name}}}"),
                        seed.path_value(&operation.path, name),
                    )
                }
                "query" => assert_ne!(
                    parameter["required"],
                    json!(true),
                    "{} needs ?{name}",
                    operation.id
                ),
                other => panic!("{}: unexpected parameter location {other}", operation.id),
            }
        }
        let body = operation.request_schema.as_ref().map(|schema| {
            let body = seed.body(&operation.id);
            let violations = contract.violations(schema, &body);
            assert!(
                violations.is_empty(),
                "{} request body: {violations:#?}",
                operation.id
            );
            body
        });
        let (status, _, response) = send(
            &app,
            &operation.method,
            &uri,
            body.as_ref(),
            &bearer(&token),
        )
        .await;
        let (expected, schema) = &operation.success;
        let mut problems = Vec::new();
        if status != *expected {
            problems.push(format!(
                "status {status}, documented {expected}: {response}"
            ));
        } else {
            problems.extend(contract.violations(schema, &response));
        }
        if !problems.is_empty() {
            failures.insert(
                format!("{} {} ({})", operation.method, operation.path, operation.id),
                problems,
            );
        }
    }
    assert!(
        failures.is_empty(),
        "the router disagrees with openapi/v1.json: {failures:#?}"
    );
}

#[tokio::test]
async fn mcp_tools_are_documented_operations() {
    let (app, state) = sqlite_app().await;
    let editor = editor_session(&state).await.unwrap();
    let token = all_scopes_token(&app, &editor).await;
    let document = served_document(&app).await;
    let (status, _, reply) = send(
        &app,
        "POST",
        "/api/mcp",
        Some(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })),
        &bearer(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let documented: Vec<String> = operations(&document)
        .into_iter()
        .map(|operation| operation.id)
        .collect();
    let tools: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools.len(), 6, "{tools:?}");
    let missing: Vec<&&str> = tools
        .iter()
        .filter(|tool| !documented.iter().any(|id| id == *tool))
        .collect();
    assert!(
        missing.is_empty(),
        "MCP tools without an operationId: {missing:?}"
    );
}

#[tokio::test]
async fn the_document_is_public_and_names_this_instance() {
    let (app, state) = sqlite_app().await;
    let editor = editor_session(&state).await.unwrap();
    let token = all_scopes_token(&app, &editor).await;
    for uri in ["/api/v1/openapi.json", "/api/openapi.json"] {
        for headers in [Vec::new(), bearer(&token)] {
            let (status, response_headers, document) = send(&app, "GET", uri, None, &headers).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {document}");
            assert_eq!(document["openapi"], "3.1.0");
            assert_eq!(document["info"]["version"], "1.0.0");
            assert_eq!(
                document["servers"],
                json!([{ "url": "http://127.0.0.1/api" }])
            );
            assert_eq!(response_headers["access-control-allow-origin"], "*");
            assert_eq!(response_headers["cache-control"], "public, max-age=300");
            assert_eq!(response_headers["content-type"], "application/json");
        }
    }
}
