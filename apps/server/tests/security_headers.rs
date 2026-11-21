use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt;
use owlee_server::{boot, router, testing_config};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

async fn app() -> Router {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owlee.sqlite");
    let _keep = Box::leak(Box::new(dir));
    router(boot(testing_config(path)).await.unwrap())
}

async fn get(app: &Router, method: &str, uri: &str) -> (StatusCode, HeaderMap, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    40000,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

/// The CSP sources for every inline script in the served page, hashed the way a browser checks
/// them. The real build has one (the theme script); the placeholder page that `build.rs` writes
/// without `apps/web/dist` has none.
fn inline_script_sources(html: &str) -> String {
    html.split("<script>")
        .skip(1)
        .map(|rest| {
            let script = &rest[..rest.find("</script>").unwrap()];
            let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script));
            format!(" 'sha256-{hash}'")
        })
        .collect()
}

fn policy(scripts: &str, frame_ancestors: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self'{scripts}; style-src 'self'; img-src 'self'; \
         object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors {frame_ancestors}"
    )
}

fn assert_common(headers: &HeaderMap) {
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["referrer-policy"],
        "strict-origin-when-cross-origin"
    );
    assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
    assert_eq!(
        headers["permissions-policy"],
        "camera=(), microphone=(), geolocation=(), payment=()"
    );
    assert!(!headers.contains_key("strict-transport-security"));
}

#[tokio::test]
async fn the_dashboard_refuses_framing_and_allows_only_its_own_scripts() {
    let app = app().await;
    let (status, headers, html) = get(&app, "GET", "/").await;
    assert_eq!(status, StatusCode::OK);
    let scripts = inline_script_sources(&html);
    assert_eq!(
        headers["content-security-policy"],
        policy(&scripts, "'none'")
    );
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_common(&headers);

    let (_, settings, _) = get(&app, "GET", "/monitors").await;
    assert_eq!(
        settings["content-security-policy"],
        policy(&scripts, "'none'")
    );
}

#[tokio::test]
async fn public_status_and_share_pages_can_be_embedded() {
    let app = app().await;
    let (_, _, html) = get(&app, "GET", "/").await;
    let scripts = inline_script_sources(&html);
    for path in ["/status/acme", "/share/site/acme"] {
        let (status, headers, _) = get(&app, "GET", path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            headers["content-security-policy"],
            policy(&scripts, "*"),
            "{path}"
        );
        assert!(!headers.contains_key("x-frame-options"), "{path}");
        assert_common(&headers);
    }
}

#[tokio::test]
async fn api_and_ingest_responses_get_the_headers_and_keep_cors() {
    let app = app().await;
    let (status, headers, _) = get(&app, "GET", "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_common(&headers);

    let (_, missing, _) = get(&app, "GET", "/api/nope").await;
    assert_eq!(missing["x-content-type-options"], "nosniff");

    for path in ["/api/receive", "/data.js"] {
        let (_, headers, _) = get(&app, "OPTIONS", path).await;
        assert_eq!(headers["access-control-allow-origin"], "*", "{path}");
        assert_eq!(headers["x-content-type-options"], "nosniff", "{path}");
    }
}
