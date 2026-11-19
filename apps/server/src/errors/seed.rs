//! Demo error apps and a few grouped events for `owlee seed`.
use owlee_errors::normalize_event;
use serde_json::{Value, json};

use super::store;
use crate::error::AppError;
use crate::seed::DemoScope;
use crate::state::AppState;

pub async fn seed_demo(state: &AppState, scope: &DemoScope) -> Result<(), AppError> {
    let checkout =
        store::create_app(&state.db, scope.website_project_id, "Checkout", "php").await?;
    let base = sample(&checkout.public_key, &checkout.id);
    for (id, message) in [
        ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "disk full on volume 3"),
        ("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "disk full on volume 9"),
        ("cccccccccccccccccccccccccccccccc", "disk full on volume 1"),
    ] {
        let mut event = base.clone();
        event["event_id"] = json!(id);
        event["exception"]["values"][0]["value"] = json!(message);
        store::ingest_event(
            &state.db,
            &state.config.app_key,
            &checkout,
            &normalize_event(event),
            1_000_000,
            state.config.ingest.error_keep_first,
            state.config.ingest.error_sample_window_secs,
        )
        .await?;
    }
    let mut rust = json!({
        "event_id": "dddddddddddddddddddddddddddddddd",
        "level": "error",
        "platform": "rust",
        "environment": "production",
        "release": "api@1.4.0",
        "exception": {
            "values": [{
                "type": "ExampleError",
                "value": "payment provider timed out",
                "mechanism": {"type": "generic", "handled": false},
                "stacktrace": {
                    "frames": [{
                        "filename": "src/pay.rs",
                        "function": "checkout::charge",
                        "lineno": 88,
                        "in_app": true,
                        "context_line": "return Err(ExampleError(\"payment provider timed out\"));"
                    }]
                }
            }]
        },
        "tags": {"service": "payments"},
        "user": {"id": "42", "email": "ada@example.com"}
    });
    rust["event_id"] = json!("dddddddddddddddddddddddddddddddd");
    let api = store::create_app(&state.db, scope.api_project_id, "API", "rust").await?;
    store::ingest_event(
        &state.db,
        &state.config.app_key,
        &api,
        &normalize_event(rust),
        1_000_000,
        state.config.ingest.error_keep_first,
        state.config.ingest.error_sample_window_secs,
    )
    .await?;
    Ok(())
}

fn sample(key: &str, app_id: &str) -> Value {
    json!({
        "event_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "level": "error",
        "platform": "php",
        "environment": "production",
        "release": "checkout@2.4.0",
        "sdk": {"name": "sentry.php", "version": "4.10.0"},
        "exception": {
            "values": [{
                "type": "RuntimeException",
                "value": "disk full on volume 3",
                "mechanism": {"type": "generic", "handled": false},
                "stacktrace": {
                    "frames": [
                        {
                            "filename": "/app/src/Http.php",
                            "function": "dispatch",
                            "lineno": 18,
                            "in_app": false
                        },
                        {
                            "filename": "/app/src/Store.php",
                            "function": "save",
                            "lineno": 42,
                            "in_app": true,
                            "context_line": "throw new RuntimeException('disk full');",
                            "pre_context": ["$ok = true;"],
                            "post_context": ["}"]
                        }
                    ]
                }
            }]
        },
        "breadcrumbs": {"values": [
            {"category": "query", "message": "insert into orders", "level": "info", "timestamp": "2026-10-21T08:00:00.000Z"},
            {"category": "http", "message": "POST /pay", "level": "info"}
        ]},
        "tags": {"runtime": "php", "region": "eu"},
        "contexts": {"runtime": {"name": "php", "version": "8.3"}},
        "user": {"id": "9", "email": "buyer@example.com"},
        "request": {"method": "POST", "url": "https://shop.example/pay"},
        "extra": {"dsn_hint": format!("http://{key}@127.0.0.1/{app_id}")}
    })
}
