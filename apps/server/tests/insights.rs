use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use owlee_server::{AppState, TestSession, boot, editor_session, router, testing_config};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn app() -> (Router, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    let state = boot(testing_config(path)).await.unwrap();
    (router(state.clone()), state)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    session: Option<&TestSession>,
    user_agent: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-owlee-workspace", session.workspace_id.to_string());
    }
    if let Some(user_agent) = user_agent {
        request = request.header("user-agent", user_agent);
    }
    if let Some(origin) = body.as_ref().and_then(origin_for) {
        request = request.header("origin", origin);
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
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json, location)
}

fn origin_for(body: &Value) -> Option<String> {
    let url = body.get("url")?.as_str()?;
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        None
    } else {
        Some(format!("{scheme}://{host}"))
    }
}

const BROWSER: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36";
const BOT: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

async fn create_site(app: &Router, session: &TestSession, domain: &str) -> Value {
    let (status, site, _) = call(
        app,
        "POST",
        "/api/sites",
        Some(json!({"project_id": session.project_id, "name": "Docs", "domain": domain})),
        Some(session),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{site}");
    site
}

#[tokio::test]
async fn sqlite_collects_pageviews_and_counts_short_link_clicks() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    let (status, script, _) = call(&app, "GET", "/data.js", None, None, Some(BROWSER)).await;
    assert_eq!(status, StatusCode::OK);
    let script = script.as_str().unwrap_or("");
    assert!(script.contains("/api/receive"), "{script}");
    assert!(script.contains("window.owleeEvent"), "{script}");

    let site = create_site(&app, &editor, "coolify.io").await;
    let site_id = site["id"].as_str().unwrap();
    let public_id = site["public_id"].as_str().unwrap();

    let (status, missing, _) = call(
        &app,
        "POST",
        "/api/receive",
        Some(json!({"site": "nope", "type": "pageview", "url": "https://coolify.io/"})),
        None,
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{missing}");

    let (status, _, _) = call(
        &app,
        "POST",
        "/api/receive",
        Some(json!({
            "site": public_id,
            "type": "pageview",
            "url": "https://coolify.io/docs?utm_source=github",
            "referrer": "https://github.com/coollabsio/coolify",
            "title": "Docs",
            "hostname": "coolify.io"
        })),
        None,
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, _, _) = call(
        &app,
        "POST",
        "/api/receive",
        Some(json!({"site": public_id, "type": "pageview", "url": "https://coolify.io/"})),
        None,
        Some(BOT),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, overview, _) = call(
        &app,
        "GET",
        &format!("/api/sites/{site_id}/overview"),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["pageviews"], 1, "bots are accepted and not stored");
    assert!(overview["series"].as_array().unwrap().len() > 1);

    let (status, pages, _) = call(
        &app,
        "GET",
        &format!("/api/sites/{site_id}/breakdown/path"),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pages}");
    assert_eq!(pages["rows"][0]["label"], "/docs", "{pages}");

    let (status, domain, _) = call(
        &app,
        "POST",
        "/api/domains",
        Some(json!({"hostname": "go.owlee.test", "enabled": true})),
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{domain}");
    let (status, link, _) = call(
        &app,
        "POST",
        "/api/links",
        Some(json!({
            "project_id": editor.project_id,
            "domain_id": domain["id"],
            "slug": "coolify",
            "destination": "https://coolify.io",
            "redirect_type": "temporary"
        })),
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let (status, _, location) = call(&app, "GET", "/go/coolify", None, None, Some(BROWSER)).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location.as_deref(), Some("https://coolify.io"));
    let (status, clicks, _) = call(
        &app,
        "GET",
        &format!("/api/links/{}/overview", link["id"].as_str().unwrap()),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{clicks}");
    assert_eq!(clicks["clicks"], 1);
}

#[tokio::test]
async fn pageviews_filter_secrets_in_the_query_and_fragment() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    let site = create_site(&app, &editor, "coolify.io").await;
    let site_id = site["id"].as_str().unwrap();
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/receive",
        Some(json!({
            "site": site["public_id"],
            "type": "pageview",
            "url": "https://coolify.io/callback?page=2&reset_token=abc#access_token=xyz&state=s"
        })),
        None,
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    for (dimension, expected) in [
        ("path", "/callback#access_token=[Filtered]&state=s"),
        ("query", "page=2&reset_token=[Filtered]"),
    ] {
        let (status, rows, _) = call(
            &app,
            "GET",
            &format!("/api/sites/{site_id}/breakdown/{dimension}"),
            None,
            Some(&editor),
            Some(BROWSER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{rows}");
        assert_eq!(rows["rows"][0]["label"], expected, "{rows}");
    }
}

#[tokio::test]
async fn postgres_collects_a_pageview_when_configured() {
    let Some(url) = std::env::var("OWLEE_APP_POSTGRES_URL")
        .ok()
        .filter(|url| !url.is_empty())
    else {
        return;
    };
    let mut config = testing_config(std::path::PathBuf::from(
        "data/ignored-insights-postgres.sqlite",
    ));
    config.database_url = Some(url);
    let state = boot(config).await.expect("postgres boot");
    let editor = editor_session(&state).await.unwrap();
    let app = router(state.clone());
    let domain = format!("pg-{}.owlee.test", owlee_platform::Id::new()).to_lowercase();
    let site = create_site(&app, &editor, &domain).await;
    let site_id = site["id"].as_str().unwrap();
    let [waiting, _] = traffic(&app, &editor, site_id).await;
    assert_eq!(waiting["waiting"], true);
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/receive",
        Some(json!({
            "site": site["public_id"],
            "type": "pageview",
            "url": format!("https://{domain}/ready")
        })),
        None,
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, overview, _) = call(
        &app,
        "GET",
        &format!("/api/sites/{}/overview", site["id"].as_str().unwrap()),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["pageviews"], 1);
    let seen = json!({
        "tracker_url": "http://127.0.0.1/data.js",
        "waiting": false,
        "visitors_24h": 1,
        "pageviews_24h": 1,
    });
    assert_eq!(traffic(&app, &editor, site_id).await, [seen.clone(), seen]);
    let stored: i64 = sqlx::query_scalar(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM insight_events WHERE site_id = $1",
    )
    .bind(site_id)
    .fetch_one(state.db.pool())
    .await
    .unwrap();
    assert_eq!(stored, 1, "the page view row is in Postgres");
}

#[tokio::test]
async fn another_workspace_cannot_reach_insights_by_id() {
    let (app, state) = app().await;
    let owner = editor_session(&state).await.unwrap();
    let outsider = editor_session(&state).await.unwrap();
    let site = create_site(&app, &owner, "coolify.io").await;
    let site_id = site["id"].as_str().unwrap();
    assert_eq!(site["project_id"], owner.project_id.to_string());
    let (status, domain, _) = call(
        &app,
        "POST",
        "/api/domains",
        Some(json!({"hostname": "go.owlee.test", "site_id": site_id})),
        Some(&owner),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{domain}");
    let domain_id = domain["id"].as_str().unwrap();
    let link_body = json!({
        "project_id": owner.project_id,
        "domain_id": domain_id,
        "slug": "coolify",
        "destination": "https://coolify.io"
    });
    let (status, link, _) = call(
        &app,
        "POST",
        "/api/links",
        Some(link_body.clone()),
        Some(&owner),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    assert_eq!(link["project_id"], owner.project_id.to_string());
    let link_id = link["id"].as_str().unwrap();

    let site_body = json!({"name": "Stolen", "domain": "evil.test"});
    for (method, uri, body) in [
        ("GET", format!("/api/sites/{site_id}"), None),
        ("GET", format!("/api/sites/{site_id}/overview"), None),
        ("GET", format!("/api/sites/{site_id}/visitors"), None),
        (
            "GET",
            format!("/api/sites/{site_id}/events/signup/properties"),
            None,
        ),
        ("DELETE", format!("/api/sites/{site_id}/analytics"), None),
        ("PUT", format!("/api/sites/{site_id}"), Some(site_body)),
        ("DELETE", format!("/api/sites/{site_id}"), None),
        ("GET", format!("/api/links/{link_id}/overview"), None),
        (
            "PUT",
            format!("/api/links/{link_id}"),
            Some(link_body.clone()),
        ),
        ("DELETE", format!("/api/links/{link_id}"), None),
        ("DELETE", format!("/api/domains/{domain_id}"), None),
    ] {
        let (status, body, _) =
            call(&app, method, &uri, body, Some(&outsider), Some(BROWSER)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
    }

    for (uri, body) in [
        (
            "/api/links",
            json!({
                "project_id": outsider.project_id,
                "domain_id": domain_id,
                "slug": "borrowed",
                "destination": "https://coolify.io"
            }),
        ),
        (
            "/api/domains",
            json!({"hostname": "go.evil.test", "site_id": site_id}),
        ),
        (
            "/api/sites",
            json!({"project_id": owner.project_id, "name": "Docs", "domain": "evil.test"}),
        ),
    ] {
        let (status, body, _) = call(
            &app,
            "POST",
            uri,
            Some(body),
            Some(&outsider),
            Some(BROWSER),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "cross-workspace POST {uri}: {body}"
        );
    }

    for uri in ["/api/sites", "/api/links", "/api/domains"] {
        let (status, rows, _) = call(&app, "GET", uri, None, Some(&outsider), Some(BROWSER)).await;
        assert_eq!(status, StatusCode::OK, "{rows}");
        assert_eq!(rows, json!([]), "{uri} lists only the caller's workspace");
    }
    let (status, site, _) = call(
        &app,
        "GET",
        &format!("/api/sites/{site_id}"),
        None,
        Some(&owner),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{site}");
    assert_eq!(site["name"], "Docs", "the outsider's update did not land");
}

#[tokio::test]
async fn project_filter_narrows_the_site_list() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    let (status, project, _) = call(
        &app,
        "POST",
        "/api/projects",
        Some(json!({"name": "Blog", "color": "#10b981"})),
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let blog = project["id"].as_str().unwrap();
    let docs = create_site(&app, &editor, "coolify.io").await;
    let (status, blog_site, _) = call(
        &app,
        "POST",
        "/api/sites",
        Some(json!({"project_id": blog, "name": "Blog", "domain": "blog.coolify.io"})),
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{blog_site}");

    let ids = |rows: &Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let (_, all, _) = call(
        &app,
        "GET",
        "/api/sites",
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(
        ids(&all),
        [
            docs["id"].as_str().unwrap(),
            blog_site["id"].as_str().unwrap()
        ]
    );
    let (status, only_blog, _) = call(
        &app,
        "GET",
        &format!("/api/sites?project={blog}"),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{only_blog}");
    assert_eq!(ids(&only_blog), [blog_site["id"].as_str().unwrap()]);

    let outsider = editor_session(&state).await.unwrap();
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/api/sites?project={}", outsider.project_id),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a foreign project filter is 404"
    );
}

#[tokio::test]
async fn another_workspace_cannot_take_a_go_slug() {
    let (app, state) = app().await;
    let owner = editor_session(&state).await.unwrap();
    let outsider = editor_session(&state).await.unwrap();
    let mut links = Vec::new();
    for (session, hostname) in [(&owner, "go.owner.test"), (&outsider, "go.outsider.test")] {
        let (status, domain, _) = call(
            &app,
            "POST",
            "/api/domains",
            Some(json!({"hostname": hostname})),
            Some(session),
            Some(BROWSER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{domain}");
        let (status, link, _) = call(
            &app,
            "POST",
            "/api/links",
            Some(json!({
                "project_id": session.project_id,
                "domain_id": domain["id"],
                "slug": "launch",
                "destination": format!("https://{hostname}/launch")
            })),
            Some(session),
            Some(BROWSER),
        )
        .await;
        links.push((status, link));
    }
    assert_eq!(links[0].0, StatusCode::CREATED, "{}", links[0].1);
    assert_eq!(links[1].0, StatusCode::CONFLICT, "{}", links[1].1);

    let (status, _, location) = call(&app, "GET", "/go/launch", None, None, Some(BROWSER)).await;
    assert!(status.is_redirection(), "{status}");
    assert_eq!(location.as_deref(), Some("https://go.owner.test/launch"));
}

async fn pageview(app: &Router, site: &Value, path: &str) {
    let domain = site["domain"].as_str().unwrap();
    let (status, body, _) = call(
        app,
        "POST",
        "/api/receive",
        Some(json!({
            "site": site["public_id"],
            "type": "pageview",
            "url": format!("https://{domain}{path}"),
            "hostname": domain
        })),
        None,
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

/// The fields the setup guide and the site list read, from the detail and from the list.
async fn traffic(app: &Router, session: &TestSession, site_id: &str) -> [Value; 2] {
    let (status, detail, _) = call(
        app,
        "GET",
        &format!("/api/sites/{site_id}"),
        None,
        Some(session),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    let (status, list, _) =
        call(app, "GET", "/api/sites", None, Some(session), Some(BROWSER)).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|site| site["id"] == site_id)
        .unwrap()
        .clone();
    [detail, listed].map(|site| {
        json!({
            "tracker_url": site["tracker_url"],
            "waiting": site["last_event_at"].is_null(),
            "visitors_24h": site["visitors_24h"],
            "pageviews_24h": site["pageviews_24h"],
        })
    })
}

#[tokio::test]
async fn site_view_reports_the_tracker_url_and_the_last_day_of_traffic() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = testing_config(dir.path().join("owlee.sqlite"));
    config.public_origin = "http://owlee.example.com:8080/".to_owned();
    let state = boot(config).await.unwrap();
    let app = router(state.clone());
    let editor = editor_session(&state).await.unwrap();
    let site = create_site(&app, &editor, "coolify.io").await;
    let site_id = site["id"].as_str().unwrap();
    let before = json!({
        "tracker_url": "http://owlee.example.com:8080/data.js",
        "waiting": true,
        "visitors_24h": 0,
        "pageviews_24h": 0,
    });
    assert_eq!(
        traffic(&app, &editor, site_id).await,
        [before.clone(), before]
    );

    let start = owlee_insights::now_secs();
    pageview(&app, &site, "/").await;
    pageview(&app, &site, "/pricing").await;
    let after = json!({
        "tracker_url": "http://owlee.example.com:8080/data.js",
        "waiting": false,
        "visitors_24h": 1,
        "pageviews_24h": 2,
    });
    assert_eq!(
        traffic(&app, &editor, site_id).await,
        [after.clone(), after]
    );
    let (_, detail, _) = call(
        &app,
        "GET",
        &format!("/api/sites/{site_id}"),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    let last = detail["last_event_at"].as_i64().unwrap();
    assert!(
        (start..=start + 5).contains(&last),
        "last_event_at {last} is the time of the page view"
    );

    let (status, _, _) = call(
        &app,
        "DELETE",
        &format!("/api/sites/{site_id}/analytics"),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let reset = json!({
        "tracker_url": "http://owlee.example.com:8080/data.js",
        "waiting": true,
        "visitors_24h": 0,
        "pageviews_24h": 0,
    });
    assert_eq!(
        traffic(&app, &editor, site_id).await,
        [reset.clone(), reset]
    );
}

#[tokio::test]
async fn a_pasted_url_is_not_stored_as_a_domain() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    for domain in ["https://coolify.io/blog", "coolify.io/blog"] {
        let (status, body, _) = call(
            &app,
            "POST",
            "/api/sites",
            Some(json!({"project_id": editor.project_id, "name": "Docs", "domain": domain})),
            Some(&editor),
            Some(BROWSER),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{domain}: {body}");
    }
    let site = create_site(&app, &editor, "WWW.Coolify.io:443").await;
    assert_eq!(site["domain"], "www.coolify.io");
}

#[tokio::test]
async fn members_can_read_a_site_but_not_change_it() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    let member = owlee_server::member_session(&state, editor.workspace_id)
        .await
        .unwrap();
    let site = create_site(&app, &editor, "coolify.io").await;
    let site_id = site["id"].as_str().unwrap();
    pageview(&app, &site, "/").await;
    for (method, uri, body) in [
        (
            "PUT",
            format!("/api/sites/{site_id}"),
            Some(json!({"name": "Renamed", "domain": "coolify.io"})),
        ),
        ("DELETE", format!("/api/sites/{site_id}/analytics"), None),
        ("DELETE", format!("/api/sites/{site_id}"), None),
        (
            "PUT",
            format!("/api/sites/{site_id}/quota"),
            Some(json!({"daily_quota": 5})),
        ),
        ("POST", format!("/api/sites/{site_id}/verify"), None),
        (
            "PUT",
            format!("/api/sites/{site_id}/public"),
            Some(
                json!({"enabled": true, "slug": site["public_slug"], "threshold": 0, "sections": []}),
            ),
        ),
    ] {
        let (status, body, _) = call(&app, method, &uri, body, Some(&member), Some(BROWSER)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
    }
    let [seen, _] = traffic(&app, &member, site_id).await;
    assert_eq!(seen["pageviews_24h"], 1, "the member's reset did not land");
    let (_, detail, _) = call(
        &app,
        "GET",
        &format!("/api/sites/{site_id}"),
        None,
        Some(&member),
        Some(BROWSER),
    )
    .await;
    assert_eq!(
        (
            detail["name"].clone(),
            detail["daily_quota"].clone(),
            detail["public_enabled"].clone()
        ),
        (json!("Docs"), json!(1_000_000), json!(false))
    );
}

async fn get_with(
    app: &Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Option<String>) {
    let mut request = Request::builder().uri(uri).header("user-agent", BROWSER);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (response.status(), location)
}

async fn create_link(app: &Router, session: &TestSession, hostname: &str, slug: &str) -> Value {
    let (status, domain, _) = call(
        app,
        "POST",
        "/api/domains",
        Some(json!({"hostname": hostname})),
        Some(session),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{domain}");
    let (status, link, _) = call(
        app,
        "POST",
        "/api/links",
        Some(json!({
            "project_id": session.project_id,
            "domain_id": domain["id"],
            "slug": slug,
            "destination": "https://example.org/landing"
        })),
        Some(session),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    link
}

#[tokio::test]
async fn requests_from_another_origin_do_not_use_the_site_bucket() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = testing_config(dir.path().join("owlee.sqlite"));
    config.ingest.analytics_rate_site_per_min = 1;
    config.ingest.analytics_rate_site_burst = 2;
    let state = boot(config).await.unwrap();
    let app = router(state.clone());
    let editor = editor_session(&state).await.unwrap();
    let site = create_site(&app, &editor, "coolify.io").await;
    let receive = |url: &'static str| {
        let app = app.clone();
        let body = json!({"site": site["public_id"], "type": "pageview", "url": url});
        async move {
            call(
                &app,
                "POST",
                "/api/receive",
                Some(body),
                None,
                Some(BROWSER),
            )
            .await
            .0
        }
    };
    for _ in 0..5 {
        assert_eq!(
            receive("https://attacker.test/").await,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(receive("https://coolify.io/").await, StatusCode::ACCEPTED);
    assert_eq!(receive("https://coolify.io/").await, StatusCode::ACCEPTED);
    assert_eq!(
        receive("https://coolify.io/").await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn clicks_past_the_limit_redirect_without_being_counted() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    let link = create_link(&app, &editor, "go.limit.test", "launch").await;
    for _ in 0..12 {
        let (status, location) = get_with(&app, "/go/launch", &[]).await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(location.as_deref(), Some("https://example.org/landing"));
    }
    let (status, location) = get_with(&app, "/launch", &[("host", "go.limit.test")]).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location.as_deref(), Some("https://example.org/landing"));
    let (_, link, _) = call(
        &app,
        "GET",
        &format!("/api/links/{}", link["id"].as_str().unwrap()),
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(link["total_clicks"], 10);
}

#[tokio::test]
async fn values_past_the_daily_cap_are_counted_as_other() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    let site = create_site(&app, &editor, "coolify.io").await;
    let link = create_link(&app, &editor, "go.cap.test", "launch").await;
    let day = owlee_insights::day(owlee_insights::now_secs());
    let mut tx = state.db.begin().await.unwrap();
    for (table, owner_column, owner, dimension) in [
        ("insight_dimension_daily", "site_id", &site["id"], "path"),
        (
            "insight_link_dimension_daily",
            "link_id",
            &link["id"],
            "referrer",
        ),
    ] {
        for index in 0..1000 {
            sqlx::query(&format!(
                "INSERT INTO {table} ({owner_column}, day, dimension, value, count) VALUES ($1, $2, $3, $4, 1)"
            ))
            .bind(owner.as_str().unwrap())
            .bind(day)
            .bind(dimension)
            .bind(format!("/p{index}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        }
    }
    tx.commit().await.unwrap();

    pageview(&app, &site, "/p7").await;
    pageview(&app, &site, "/new-one").await;
    pageview(&app, &site, "/new-two").await;
    for referrer in ["https://one.test/", "https://two.test/"] {
        let (status, _) = get_with(&app, "/go/launch", &[("referer", referrer)]).await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    }

    let counts = |table: &'static str, dimension: &'static str| {
        let pool = state.db.pool().clone();
        async move {
            let rows: Vec<(String, i64)> = sqlx::query_as(&format!(
                "SELECT value, count FROM {table} WHERE dimension = $1 \
                 AND (count > 1 OR value NOT LIKE '/p%') ORDER BY value"
            ))
            .bind(dimension)
            .fetch_all(&pool)
            .await
            .unwrap();
            let total: i64 = sqlx::query_scalar(&format!(
                "SELECT CAST(COUNT(*) AS BIGINT) FROM {table} WHERE dimension = $1"
            ))
            .bind(dimension)
            .fetch_one(&pool)
            .await
            .unwrap();
            (rows, total)
        }
    };
    assert_eq!(
        counts("insight_dimension_daily", "path").await,
        (vec![("(other)".to_owned(), 2), ("/p7".to_owned(), 2)], 1001)
    );
    assert_eq!(
        counts("insight_link_dimension_daily", "referrer").await,
        (vec![("(other)".to_owned(), 2)], 1001)
    );
}

#[tokio::test]
async fn public_stats_are_limited_per_client() {
    let (app, state) = app().await;
    let editor = editor_session(&state).await.unwrap();
    let site = create_site(&app, &editor, "coolify.io").await;
    let (status, body, _) = call(
        &app,
        "PUT",
        &format!("/api/sites/{}/public", site["id"].as_str().unwrap()),
        Some(json!({"enabled": true, "slug": "open-stats", "threshold": 0, "sections": []})),
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let uri = "/api/public/sites/open-stats/overview";
    for _ in 0..60 {
        assert_eq!(get_with(&app, uri, &[]).await.0, StatusCode::OK);
    }
    assert_eq!(
        get_with(&app, uri, &[]).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn a_link_hostname_redirects_only_after_its_txt_record_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = testing_config(dir.path().join("owlee.sqlite"));
    config.environment = owlee_platform::EnvironmentMode::Production;
    config.public_origin = "http://owlee.example.com".to_owned();
    let mut state = boot(config).await.unwrap();
    let dns = owlee_server::MapResolver::default();
    state.dns = std::sync::Arc::new(dns.clone());
    let app = router(state.clone());
    let editor = editor_session(&state).await.unwrap();

    let (status, own, _) = call(
        &app,
        "POST",
        "/api/domains",
        Some(json!({"hostname": "Owlee.Example.com"})),
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{own}");

    let link = create_link(&app, &editor, "go.brand.test", "launch").await;
    let domain_id = link["domain_id"].as_str().unwrap();
    for slug in ["api", "login/now"] {
        let (status, body, _) = call(
            &app,
            "POST",
            "/api/links",
            Some(json!({
                "project_id": editor.project_id,
                "domain_id": domain_id,
                "slug": slug,
                "destination": "https://example.org/phish"
            })),
            Some(&editor),
            Some(BROWSER),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{slug}: {body}");
    }

    let on_host = [("host", "go.brand.test")];
    let (status, location) = get_with(&app, "/launch", &on_host).await;
    assert!(!status.is_redirection(), "{status}");
    assert_eq!(location, None);

    let verify = format!("/api/domains/{domain_id}/verify");
    let (status, body, _) = call(&app, "POST", &verify, None, Some(&editor), Some(BROWSER)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A domain made before verification existed has no token yet.
    sqlx::query("UPDATE insight_domains SET verify_token = ''")
        .execute(state.db.pool())
        .await
        .unwrap();
    let (_, domains, _) = call(
        &app,
        "GET",
        "/api/domains",
        None,
        Some(&editor),
        Some(BROWSER),
    )
    .await;
    assert_eq!(domains[0]["verified"], false);
    assert_eq!(domains[0]["redirect_open"], false);
    assert_eq!(domains[0]["verify_name"], "_owlee-verify.go.brand.test");
    let value = domains[0]["verify_value"].as_str().unwrap().to_owned();
    assert!(value.len() > "owlee-verify=".len(), "{value}");
    dns.set("_owlee-verify.go.brand.test", vec![value]);
    let (status, body, _) = call(&app, "POST", &verify, None, Some(&editor), Some(BROWSER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["verified"], true);

    let (status, location) = get_with(&app, "/launch", &on_host).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location.as_deref(), Some("https://example.org/landing"));

    dns.set("_owlee-verify.go.brand.test", Vec::new());
    assert_eq!(owlee_server::recheck_verified(&state).await.unwrap(), 1);
    let (status, location) = get_with(&app, "/launch", &on_host).await;
    assert!(!status.is_redirection(), "{status}");
    assert_eq!(location, None);
}
