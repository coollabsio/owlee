use std::io::Write;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use flate2::Compression;
use flate2::write::{DeflateEncoder, GzEncoder};
use http_body_util::BodyExt;
use owlee_server::{
    AppState, TestSession, boot, editor_session, member_session, router, testing_config,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn app() -> (Router, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    let state = boot(fast_ingest(testing_config(path))).await.unwrap();
    (router(state.clone()), state)
}

/// Sends `session`'s cookie and workspace header when given.
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    session: Option<&TestSession>,
    extra: &[(&str, &str)],
) -> (StatusCode, Value, Option<String>) {
    let mut request = Request::builder().method(method).uri(uri);
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
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let allow = headers
        .get("access-control-allow-origin")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let cookie = session_cookie(&headers);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let _ = allow;
    (status, json, cookie.or(allow))
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

/// The sort test ingests 203 events from one address in one burst.
fn fast_ingest(mut config: owlee_platform::Config) -> owlee_platform::Config {
    config.ingest.error_rate_ip_burst = 1_000;
    config.ingest.error_rate_project_burst = 1_000;
    config
}

fn envelope(event_json: &str) -> Vec<u8> {
    let header = format!(r#"{{"type":"event","length":{}}}"#, event_json.len());
    let mut body = b"{}\n".to_vec();
    body.extend_from_slice(header.as_bytes());
    body.push(b'\n');
    body.extend_from_slice(event_json.as_bytes());
    body.push(b'\n');
    body.extend_from_slice(br#"{"type":"session"}"#);
    body.push(b'\n');
    body.extend_from_slice(br#"{"sid":"s1","status":"ok"}"#);
    body.push(b'\n');
    body.extend_from_slice(br#"{"type":"client_report","length":2}"#);
    body.push(b'\n');
    body.extend_from_slice(br#"{}"#);
    body.push(b'\n');
    body
}

fn hex_id() -> String {
    owlee_platform::Id::new().to_string().replace('-', "")
}

fn sample() -> Value {
    json!({
        "event_id": "11111111111111111111111111111111",
        "level": "error",
        "platform": "php",
        "environment": "production",
        "release": "php@1",
        "sdk": {"name": "sentry.php", "version": "4.10.0"},
        "exception": {
            "values": [{
                "type": "RuntimeException",
                "value": "disk full on volume 3",
                "stacktrace": {
                    "frames": [{
                        "filename": "/app/src/Store.php",
                        "function": "save",
                        "lineno": 42,
                        "in_app": true,
                        "context_line": "throw new RuntimeException('disk full');",
                        "pre_context": ["$ok = true;"],
                        "post_context": ["}"]
                    }]
                }
            }]
        },
        "tags": {"runtime": "php"},
        "contexts": {"runtime": {"name": "php", "version": "8.3"}},
        "user": {"id": "9"},
        "request": {"method": "POST", "url": "http://app/pay"}
    })
}

/// Creates an app in the session's first project.
async fn create_app(app: &Router, session: &TestSession, name: &str) -> Value {
    let (status, created, _) = call(
        app,
        "POST",
        "/api/error-apps",
        json!({"name": name, "platform": "php", "project_id": session.project_id})
            .to_string()
            .into_bytes(),
        Some(session),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created
}

async fn store(app: &Router, created: &Value, event: &Value) {
    let id = created["id"].as_str().unwrap();
    let key = created["publicKey"].as_str().unwrap();
    let (status, body, _) = call(
        app,
        "POST",
        &format!("/api/{id}/store/"),
        event.to_string().into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            (
                "x-sentry-auth",
                &format!("Sentry sentry_key={key}, sentry_version=7"),
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn exercise(app: Router, state: AppState) {
    let cookie = editor_session(&state).await.unwrap();

    let (status, _, _) = call(&app, "GET", "/api/error-apps", Vec::new(), None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let project = create_app(&app, &cookie, "Checkout").await;
    assert_eq!(project["projectId"], json!(cookie.project_id.to_string()));
    let id = project["id"].as_str().unwrap();
    let key = project["publicKey"].as_str().unwrap();
    assert!(project["dsn"].as_str().unwrap().contains(key));
    assert!(
        project["dsn"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/{id}"))
    );
    assert_eq!(project["lastEventAt"], Value::Null);
    assert_eq!(project["issueCount"], json!(0));
    assert_eq!(project["eventCount"], json!(0));

    let mut event = sample();
    let id1 = hex_id();
    let id2 = hex_id();
    let id3 = hex_id();
    let id4 = hex_id();
    event["event_id"] = json!(id1);
    let body = envelope(&event.to_string());
    let (status, _, _) = call(
        &app,
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
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, after, _) = call(
        &app,
        "GET",
        &format!("/api/error-apps/{id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert!(after["lastEventAt"].is_string(), "{after}");
    assert_eq!(after["issueCount"], json!(1));
    assert_eq!(after["eventCount"], json!(1));
    let (_, listed, _) = call(
        &app,
        "GET",
        "/api/error-apps",
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    let listed = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == json!(id))
        .unwrap();
    assert_eq!(listed["eventCount"], json!(1));

    let mut second = event.clone();
    second["event_id"] = json!(id2);
    second["exception"]["values"][0]["value"] = json!("disk full on volume 9");
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/api/{id}/store/"),
        second.to_string().into_bytes(),
        None,
        &[
            ("content-type", "application/json"),
            (
                "authorization",
                &format!("Sentry sentry_key={key}, sentry_version=7"),
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut third = event.clone();
    third["event_id"] = json!(id3);
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&envelope(&third.to_string())).unwrap();
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/api/{id}/envelope/"),
        gzip.finish().unwrap(),
        None,
        &[
            ("content-type", "application/x-sentry-envelope"),
            ("content-encoding", "gzip"),
            (
                "x-sentry-auth",
                &format!("Sentry sentry_key={key}, sentry_version=7"),
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut fourth = event.clone();
    fourth["event_id"] = json!(id4);
    let mut deflate = DeflateEncoder::new(Vec::new(), Compression::default());
    deflate.write_all(fourth.to_string().as_bytes()).unwrap();
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/api/{id}/store/"),
        deflate.finish().unwrap(),
        None,
        &[
            ("content-type", "application/json"),
            ("content-encoding", "deflate"),
            (
                "x-sentry-auth",
                &format!("Sentry sentry_key={key}, sentry_version=7"),
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, feed, _) = call(
        &app,
        "GET",
        &format!("/api/error-issues?app_id={id}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    assert_eq!(feed["issues"].as_array().unwrap().len(), 1, "{feed}");
    assert_eq!(feed["issues"][0]["eventCount"], 4);
    assert_eq!(feed["issues"][0]["appId"], json!(id));
    assert_eq!(feed["issues"][0]["appName"], json!("Checkout"));
    assert_eq!(feed["issues"][0]["appSlug"], project["slug"]);
    assert_eq!(
        feed["counts"],
        json!({"unresolved": 1, "resolved": 0, "ignored": 0})
    );
    assert_eq!(feed["nextCursor"], Value::Null);
    assert!(
        feed["issues"][0]["title"]
            .as_str()
            .unwrap()
            .contains("RuntimeException")
    );
    let slug = project["slug"].as_str().unwrap();
    let prefix: String = slug
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect();
    assert!(
        feed["issues"][0]["shortId"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}-", prefix.to_uppercase())),
        "{feed}"
    );

    let (status, detail, _) = call(
        &app,
        "GET",
        &format!("/api/error-events/{id1}"),
        Vec::new(),
        Some(&cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(
        detail.to_string().contains("preContext") || detail.to_string().contains("pre_context")
    );

    let (status, _, allow) = call(
        &app,
        "OPTIONS",
        &format!("/api/{id}/envelope/"),
        Vec::new(),
        None,
        &[
            ("origin", "http://localhost:3000"),
            (
                "access-control-request-headers",
                "x-sentry-auth, content-type",
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(allow.as_deref(), Some("*"));

    feed_pages_reach_every_issue(&app, &state, &cookie).await;
    events_page_and_step_through_every_event(&app, &cookie).await;
    detail_reports_true_edges(&app, &cookie).await;
    members_only_read(&app, &state, &cookie).await;
    other_workspaces_get_404(&app, &state, &cookie).await;
    project_filter_narrows_apps_and_issues(&app, &state).await;
}

/// A range that hides the oldest event must still name it as the issue's first event.
async fn detail_reports_true_edges(app: &Router, cookie: &TestSession) {
    let project = create_app(app, cookie, "Edges").await;
    let project_id = project["id"].as_str().unwrap();
    let key = project["publicKey"].as_str().unwrap();
    let old_id = hex_id();
    let new_id = hex_id();
    let old_time = (chrono::Utc::now() - chrono::Duration::days(20)).timestamp();
    let new_time = chrono::Utc::now().timestamp();
    let sends = [
        (
            &old_id,
            old_time,
            json!({"email": "ada@example.com"}),
            "Firefox",
        ),
        (&new_id, new_time, json!({"id": "7"}), "Chrome"),
    ];
    for (event_id, timestamp, user, browser) in sends {
        let mut event = sample();
        event["event_id"] = json!(event_id);
        event["timestamp"] = json!(timestamp);
        event["user"] = user;
        event["tags"] = json!({"browser": browser, "runtime": "php"});
        let (status, _, _) = call(
            app,
            "POST",
            &format!("/api/{project_id}/store/"),
            event.to_string().into_bytes(),
            None,
            &[
                ("content-type", "application/json"),
                (
                    "x-sentry-auth",
                    &format!("Sentry sentry_key={key}, sentry_version=7"),
                ),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (_, feed, _) = call(
        app,
        "GET",
        &format!("/api/error-issues?app_id={project_id}&since=30d"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    let issue_id = feed["issues"][0]["id"].as_str().unwrap();

    let (status, detail, _) = call(
        app,
        "GET",
        &format!("/api/error-issues/{issue_id}?range=24h"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["firstEventId"], json!(old_id));
    assert_eq!(detail["latestEventId"], json!(new_id));
    assert_eq!(detail["eventCount"], json!(2));
    assert_eq!(detail["environments"], json!(["production"]));
    assert_eq!(detail["periodEventCount"], json!(1));
    assert_eq!(detail["userCount"], json!(1));
    let (_, events, _) = call(
        app,
        "GET",
        &format!("/api/error-issues/{issue_id}/events?range=24h"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    let events: Vec<(Value, Value)> = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| (event["id"].clone(), event["user"].clone()))
        .collect();
    assert_eq!(events, vec![(json!(new_id), json!("7"))]);
    assert_eq!(
        detail["tags"],
        json!([
            {"key": "browser", "values": [{"value": "Chrome", "count": 1}]},
            {"key": "runtime", "values": [{"value": "php", "count": 1}]},
        ])
    );

    let (_, detail, _) = call(
        app,
        "GET",
        &format!("/api/error-issues/{issue_id}?range=30d"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    assert_eq!(detail["firstEventId"], json!(old_id));
    assert_eq!(detail["periodEventCount"], json!(2));
    assert_eq!(detail["userCount"], json!(2));
    let (_, events, _) = call(
        app,
        "GET",
        &format!("/api/error-issues/{issue_id}/events?range=30d"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    assert_eq!(events["items"][1]["user"], json!("ada@example.com"));
    assert_eq!(
        detail["tags"],
        json!([
            {"key": "browser", "values": [{"value": "Chrome", "count": 1}, {"value": "Firefox", "count": 1}]},
            {"key": "runtime", "values": [{"value": "php", "count": 2}]},
        ])
    );
}

/// Follows `nextCursor` from the first page to the last.
async fn walk(app: &Router, session: &TestSession, uri: &str, list: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut cursor = String::new();
    loop {
        let (status, page, _) = call(
            app,
            "GET",
            &format!("{uri}&cursor={cursor}"),
            Vec::new(),
            Some(session),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{uri} {page}");
        let items = page[list].as_array().unwrap();
        ids.extend(
            items
                .iter()
                .map(|item| item["id"].as_str().unwrap().to_owned()),
        );
        match page["nextCursor"].as_str() {
            Some(next) => {
                assert!(!items.is_empty(), "{uri}");
                cursor = next.to_owned();
            }
            None => return ids,
        }
    }
}

async fn issue_ids(state: &AppState, app_id: &str) -> Vec<String> {
    let mut ids: Vec<String> = sqlx::query("SELECT id FROM issues WHERE app_id = $1")
        .bind(app_id)
        .fetch_all(state.db.pool())
        .await
        .unwrap()
        .iter()
        .map(|row| sqlx::Row::get(row, "id"))
        .collect();
    ids.sort();
    ids
}

/// 201 issues span five pages under every sort, each issue exactly once, and
/// one busy issue older than the rest still leads `sort=events`.
async fn feed_pages_reach_every_issue(app: &Router, state: &AppState, cookie: &TestSession) {
    let project = create_app(app, cookie, "Sorting").await;
    let id = project["id"].as_str().unwrap();
    let key = project["publicKey"].as_str().unwrap();
    let auth = format!("Sentry sentry_key={key}, sentry_version=7");
    let hour_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        - 3600.0;
    let mut events = Vec::new();
    for _ in 0..3 {
        let mut event = sample();
        event["fingerprint"] = json!(["busy"]);
        event["exception"]["values"][0]["value"] = json!("busy volume");
        event["timestamp"] = json!(hour_ago);
        events.push(event);
    }
    for index in 0..200 {
        let mut event = sample();
        event["fingerprint"] = json!([format!("quiet-{index}")]);
        events.push(event);
    }
    for mut event in events {
        event["event_id"] = json!(hex_id());
        let (status, _, _) = call(
            app,
            "POST",
            &format!("/api/{id}/store/"),
            event.to_string().into_bytes(),
            None,
            &[
                ("content-type", "application/json"),
                ("x-sentry-auth", &auth),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let all = issue_ids(state, id).await;
    assert_eq!(all.len(), 201);

    let feed_uri = format!("/api/error-issues?app_id={id}&since=14d&sort=events&status=unresolved");
    let (status, feed, _) = call(app, "GET", &feed_uri, Vec::new(), Some(cookie), &[]).await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    let issues = feed["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 50);
    assert_eq!(issues[0]["title"], json!("RuntimeException: busy volume"));
    assert_eq!(issues[0]["periodEventCount"], json!(3));
    assert_eq!(issues[1]["periodEventCount"], json!(1));
    assert_eq!(
        feed["counts"],
        json!({"unresolved": 201, "resolved": 0, "ignored": 0})
    );
    let busy = issues[0]["id"].as_str().unwrap().to_owned();

    let by_events = walk(app, cookie, &feed_uri, "issues").await;
    let mut quiet: Vec<String> = all
        .iter()
        .filter(|issue| **issue != busy)
        .cloned()
        .collect();
    quiet.sort_by(|left, right| right.cmp(left));
    assert_eq!(by_events[0], busy);
    assert_eq!(by_events[1..], quiet[..], "ties run by id, newest id first");
    for sort in ["last_seen", "age", "users", "title", "priority"] {
        for order in ["", "asc", "desc"] {
            let mut walked = walk(
                app,
                cookie,
                &format!("/api/error-issues?app_id={id}&sort={sort}&order={order}&limit=37"),
                "issues",
            )
            .await;
            walked.sort();
            assert_eq!(walked, all, "sort={sort} order={order}");
        }
    }
    let ascending = walk(app, cookie, &format!("{feed_uri}&order=asc"), "issues").await;
    assert_eq!(
        ascending.last(),
        Some(&busy),
        "order=asc flips the events sort"
    );

    let (status, _, _) = call(
        app,
        "POST",
        "/api/error-issues/bulk",
        json!({"ids": [busy], "status": "resolved"})
            .to_string()
            .into_bytes(),
        Some(cookie),
        &[("content-type", "application/json")],
    )
    .await;
    assert!(status.is_success(), "{status}");
    let (_, feed, _) = call(app, "GET", &feed_uri, Vec::new(), Some(cookie), &[]).await;
    assert_eq!(
        feed["counts"],
        json!({"unresolved": 200, "resolved": 1, "ignored": 0})
    );
    let unresolved = walk(app, cookie, &format!("{feed_uri}&limit=30"), "issues").await;
    assert_eq!(unresolved, quiet, "the status filter holds across pages");
    let resolved = walk(
        app,
        cookie,
        &format!("/api/error-issues?app_id={id}&status=resolved&limit=1"),
        "issues",
    )
    .await;
    assert_eq!(resolved, vec![busy]);

    let (_, first, _) = call(
        app,
        "GET",
        &format!("/api/error-issues?app_id={id}&limit=2"),
        Vec::new(),
        Some(cookie),
        &[],
    )
    .await;
    let text_cursor = first["nextCursor"].as_str().unwrap();
    for (query, why) in [
        ("cursor=not-a-cursor".to_owned(), "garbage"),
        ("cursor=e30".to_owned(), "an empty object"),
        (
            format!("cursor={text_cursor}&sort=events"),
            "a last-seen cursor on a count sort",
        ),
    ] {
        let (status, body, _) = call(
            app,
            "GET",
            &format!("/api/error-issues?app_id={id}&{query}"),
            Vec::new(),
            Some(cookie),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }
}

/// Five events, the most one issue keeps per window, page out two at a time, and Older and Newer step
/// through all of them under the same filters as the list.
async fn events_page_and_step_through_every_event(app: &Router, cookie: &TestSession) {
    let project = create_app(app, cookie, "Paging").await;
    let app_id = project["id"].as_str().unwrap();
    let now = chrono::Utc::now().timestamp();
    let mut sent = Vec::new();
    for index in 0..5_i64 {
        let mut event = sample();
        let event_id = hex_id();
        event["event_id"] = json!(event_id);
        event["fingerprint"] = json!(["paging"]);
        // Two events share each second, so the id breaks the tie.
        event["timestamp"] = json!(now - 600 + index / 2);
        event["environment"] = json!(if index % 3 == 0 {
            "staging"
        } else {
            "production"
        });
        store(app, &project, &event).await;
        sent.push((now - 600 + index / 2, event_id, index % 3 == 0));
    }
    sent.sort_by(|left, right| (right.0, &right.1).cmp(&(left.0, &left.1)));
    let newest_first: Vec<String> = sent.iter().map(|(_, id, _)| id.clone()).collect();
    let staging: Vec<String> = sent
        .iter()
        .filter(|(_, _, staging)| *staging)
        .map(|(_, id, _)| id.clone())
        .collect();
    assert_eq!(staging.len(), 2);
    let issue = first_issue(app, cookie, app_id).await;
    let issue_id = issue["id"].as_str().unwrap();

    let events = format!("/api/error-issues/{issue_id}/events");
    let walked = walk(app, cookie, &format!("{events}?limit=2"), "items").await;
    assert_eq!(walked, newest_first);
    let walked = walk(
        app,
        cookie,
        &format!("{events}?limit=1&environment=staging"),
        "items",
    )
    .await;
    assert_eq!(walked, staging, "the environment filter holds across pages");

    for (filter, expected) in [("", &newest_first), ("environment=staging", &staging)] {
        let mut stepped = vec![expected[0].clone()];
        loop {
            let current = stepped.last().unwrap();
            let (status, near, _) = call(
                app,
                "GET",
                &format!("{events}/{current}/neighbors?{filter}"),
                Vec::new(),
                Some(cookie),
                &[],
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{near}");
            if stepped.len() > 1 {
                assert_eq!(near["newerId"], json!(stepped[stepped.len() - 2]));
            } else {
                assert_eq!(near["newerId"], Value::Null);
            }
            match near["olderId"].as_str() {
                Some(older) => stepped.push(older.to_owned()),
                None => break,
            }
        }
        assert_eq!(&stepped, expected, "filter {filter:?}");
    }

    let other = create_app(app, cookie, "Other").await;
    let mut event = sample();
    event["event_id"] = json!(hex_id());
    store(app, &other, &event).await;
    let stranger = first_issue(app, cookie, other["id"].as_str().unwrap()).await;
    let stranger_event = stranger["id"].as_str().unwrap();
    for (uri, expected) in [
        (format!("{events}?cursor=%25%25"), StatusCode::BAD_REQUEST),
        (
            format!("{events}/{}/neighbors", event["event_id"].as_str().unwrap()),
            StatusCode::NOT_FOUND,
        ),
        (
            format!(
                "/api/error-issues/{stranger_event}/events/{}/neighbors",
                newest_first[0]
            ),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, body, _) = call(app, "GET", &uri, Vec::new(), Some(cookie), &[]).await;
        assert_eq!(status, expected, "{uri}: {body}");
    }
}

fn json_body(value: Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

const JSON: &[(&str, &str)] = &[("content-type", "application/json")];

/// The first issue of an app, as its own workspace sees it.
async fn first_issue(app: &Router, session: &TestSession, app_id: &str) -> Value {
    let (status, feed, _) = call(
        app,
        "GET",
        &format!("/api/error-issues?app_id={app_id}"),
        Vec::new(),
        Some(session),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    feed["issues"][0].clone()
}

/// A read-only member sees the workspace's apps but cannot change them.
async fn members_only_read(app: &Router, state: &AppState, editor: &TestSession) {
    let member = member_session(state, editor.workspace_id).await.unwrap();
    let created = create_app(app, editor, "Readable").await;
    let id = created["id"].as_str().unwrap();
    let mut event = sample();
    event["event_id"] = json!(hex_id());
    store(app, &created, &event).await;
    let issue = first_issue(app, editor, id).await;
    let issue_id = issue["id"].as_str().unwrap();

    let (status, listed, _) = call(
        app,
        "GET",
        "/api/error-apps",
        Vec::new(),
        Some(&member),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == json!(id))
    );
    let (status, _, _) = call(
        app,
        "GET",
        &format!("/api/error-issues/{issue_id}"),
        Vec::new(),
        Some(&member),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let body = json!({"name": "Nope", "platform": "php", "project_id": editor.project_id});
    let writes = [
        ("POST", "/api/error-apps".to_owned(), body),
        (
            "PUT",
            format!("/api/error-apps/{id}/quota"),
            json!({"daily_quota": 5}),
        ),
        ("DELETE", format!("/api/error-apps/{id}"), Value::Null),
        (
            "PATCH",
            format!("/api/error-issues/{issue_id}"),
            json!({"status": "resolved"}),
        ),
        (
            "POST",
            "/api/error-issues/bulk".to_owned(),
            json!({"ids": [issue_id], "status": "resolved"}),
        ),
    ];
    for (method, uri, body) in writes {
        let (status, _, _) = call(app, method, &uri, json_body(body), Some(&member), JSON).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

/// Ids from another workspace answer 404 and change nothing.
async fn other_workspaces_get_404(app: &Router, state: &AppState, owner: &TestSession) {
    let created = create_app(app, owner, "Private").await;
    let id = created["id"].as_str().unwrap();
    let event_id = hex_id();
    let mut event = sample();
    event["event_id"] = json!(event_id);
    store(app, &created, &event).await;
    let issue = first_issue(app, owner, id).await;
    let issue_id = issue["id"].as_str().unwrap();
    let short_id = issue["shortId"].as_str().unwrap();

    let outsider = editor_session(state).await.unwrap();
    let foreign = [
        ("GET", format!("/api/error-apps/{id}"), Value::Null),
        (
            "PUT",
            format!("/api/error-apps/{id}/quota"),
            json!({"daily_quota": 5}),
        ),
        ("DELETE", format!("/api/error-apps/{id}"), Value::Null),
        ("GET", format!("/api/error-issues/{issue_id}"), Value::Null),
        ("GET", format!("/api/error-issues/{short_id}"), Value::Null),
        (
            "PATCH",
            format!("/api/error-issues/{issue_id}"),
            json!({"status": "resolved"}),
        ),
        ("GET", format!("/api/error-events/{event_id}"), Value::Null),
        (
            "GET",
            format!("/api/error-apps?project={}", owner.project_id),
            Value::Null,
        ),
        (
            "POST",
            "/api/error-apps".to_owned(),
            json!({"name": "Planted", "platform": "php", "project_id": owner.project_id}),
        ),
    ];
    for (method, uri, body) in foreign {
        let (status, _, _) = call(app, method, &uri, json_body(body), Some(&outsider), JSON).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let (status, bulk, _) = call(
        app,
        "POST",
        "/api/error-issues/bulk",
        json_body(json!({"ids": [issue_id, short_id], "status": "resolved"})),
        Some(&outsider),
        JSON,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bulk}");
    assert_eq!(bulk, json!({"updated": 0}));
    let (_, listed, _) = call(
        app,
        "GET",
        "/api/error-apps",
        Vec::new(),
        Some(&outsider),
        &[],
    )
    .await;
    assert_eq!(listed, json!([]));
    let (_, feed, _) = call(
        app,
        "GET",
        &format!("/api/error-issues?app_id={id}"),
        Vec::new(),
        Some(&outsider),
        &[],
    )
    .await;
    assert_eq!(feed["issues"], json!([]));
    assert_eq!(feed["environments"], json!([]));
    assert_eq!(
        feed["counts"],
        json!({"unresolved": 0, "resolved": 0, "ignored": 0})
    );

    let borrowed_header = TestSession {
        cookie: outsider.cookie.clone(),
        user_id: outsider.user_id,
        workspace_id: owner.workspace_id,
        project_id: owner.project_id,
    };
    let (status, _, _) = call(
        app,
        "GET",
        &format!("/api/error-apps/{id}"),
        Vec::new(),
        Some(&borrowed_header),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let after = first_issue(app, owner, id).await;
    assert_eq!(after["status"], json!("unresolved"));
    let (status, kept, _) = call(
        app,
        "GET",
        &format!("/api/error-apps/{id}"),
        Vec::new(),
        Some(owner),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["quotaOverride"], Value::Null);
    assert_eq!(kept["eventCount"], json!(1));
    let (status, _, _) = call(
        app,
        "GET",
        &format!("/api/error-events/{event_id}"),
        Vec::new(),
        Some(owner),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// `?project=` keeps apps and issues of that project only.
async fn project_filter_narrows_apps_and_issues(app: &Router, state: &AppState) {
    let owner = editor_session(state).await.unwrap();
    let (status, other, _) = call(
        app,
        "POST",
        "/api/projects",
        json_body(json!({"name": "Mobile"})),
        Some(&owner),
        JSON,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other_project = other["id"].as_str().unwrap();
    let web = create_app(app, &owner, "Web").await;
    let (status, ios, _) = call(
        app,
        "POST",
        "/api/error-apps",
        json_body(json!({"name": "iOS", "platform": "other", "project_id": other_project})),
        Some(&owner),
        JSON,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ios}");
    assert_eq!(ios["projectId"], json!(other_project));
    for created in [&web, &ios] {
        let mut event = sample();
        event["event_id"] = json!(hex_id());
        store(app, created, &event).await;
    }

    let names = |listed: &Value| -> Vec<String> {
        listed
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, all, _) = call(app, "GET", "/api/error-apps", Vec::new(), Some(&owner), &[]).await;
    assert_eq!(names(&all), ["Web", "iOS"]);
    let (_, narrowed, _) = call(
        app,
        "GET",
        &format!("/api/error-apps?project={other_project}"),
        Vec::new(),
        Some(&owner),
        &[],
    )
    .await;
    assert_eq!(names(&narrowed), ["iOS"]);

    let (_, feed, _) = call(
        app,
        "GET",
        "/api/error-issues",
        Vec::new(),
        Some(&owner),
        &[],
    )
    .await;
    assert_eq!(feed["counts"]["unresolved"], json!(2), "{feed}");
    let (status, feed, _) = call(
        app,
        "GET",
        &format!("/api/error-issues?project={other_project}"),
        Vec::new(),
        Some(&owner),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    let issues = feed["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "{feed}");
    assert_eq!(issues[0]["appName"], json!("iOS"));
    assert_eq!(feed["counts"]["unresolved"], json!(1));
}

#[tokio::test]
async fn sqlite_ingest_groups_events() {
    let (app, state) = app().await;
    exercise(app, state).await;
}

#[tokio::test]
async fn postgres_ingest_groups_events() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| url.starts_with("postgres"))
    else {
        return;
    };
    let mut config = testing_config(std::path::PathBuf::from(
        "data/ignored-errors-postgres.sqlite",
    ));
    config.database_url = Some(url);
    let state = boot(fast_ingest(config)).await.expect("postgres boot");
    exercise(router(state.clone()), state).await;
}
