//! The OpenAPI document of the token API, served without a login so agents
//! and code generators can fetch it from any instance.

use std::sync::LazyLock;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use crate::state::AppState;

static V1: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../openapi/v1.json")).expect("openapi/v1.json is valid JSON")
});

/// `/openapi.json` is the newest version, so it moves to v2 when v2 exists.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/openapi.json", get(v1))
        .route("/openapi.json", get(v1))
}

async fn v1(State(state): State<AppState>) -> Response {
    let mut document = V1.clone();
    let origin = state.config.public_origin.trim_end_matches('/');
    document["servers"] = json!([{ "url": format!("{origin}/api") }]);
    let mut response = Json(document).into_response();
    let headers = response.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    response
}
