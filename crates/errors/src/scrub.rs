//! Removes secrets and client addresses from a Sentry event before it is stored.
//!
//! `user.id`, `user.email`, and `user.username` stay. The operator chooses to
//! send them through the SDK, and privacy requests find events by them.

use serde_json::{Map, Value};

const FILTERED: &str = "[Filtered]";

/// Matched anywhere in a key after lowercasing and mapping `-` to `_`.
const SENSITIVE: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "cookie",
    "session",
    "csrf",
    "xsrf",
    "credit_card",
    "card_number",
    "private_key",
    "access_key",
];

/// Request headers that carry the client address. Matched exactly.
const ADDRESS_HEADERS: &[&str] = &[
    "x_forwarded_for",
    "x_real_ip",
    "forwarded",
    "cf_connecting_ip",
    "true_client_ip",
];

pub fn scrub_raw_event(raw: &mut Value) {
    if let Some(user) = raw.get_mut("user").and_then(Value::as_object_mut) {
        user.remove("ip_address");
    }
    if let Some(request) = raw.get_mut("request").and_then(Value::as_object_mut) {
        request.remove("cookies");
        if let Some(env) = request.get_mut("env").and_then(Value::as_object_mut) {
            env.remove("REMOTE_ADDR");
        }
        if let Some(Value::String(query)) = request.get_mut("query_string") {
            *query = scrub_query(query);
        }
        if let Some(Value::String(url)) = request.get_mut("url")
            && let Some((base, query)) = url.split_once('?')
        {
            *url = format!("{base}?{}", scrub_query(query));
        }
    }
    scrub_value(raw);
}

fn is_sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    // `auth` alone, because as a substring it would match `author`.
    key == "auth"
        || ADDRESS_HEADERS.contains(&key.as_str())
        || SENSITIVE.iter().any(|word| key.contains(word))
}

fn scrub_value(value: &mut Value) {
    match value {
        Value::Object(map) => scrub_map(map),
        Value::Array(items) => {
            for item in items {
                if let Some([Value::String(key), value]) =
                    item.as_array_mut().map(Vec::as_mut_slice)
                    && is_sensitive(key)
                {
                    *value = Value::String(FILTERED.to_owned());
                    continue;
                }
                scrub_value(item);
            }
        }
        _ => {}
    }
}

fn scrub_map(map: &mut Map<String, Value>) {
    for (key, value) in map.iter_mut() {
        if is_sensitive(key) {
            *value = Value::String(FILTERED.to_owned());
        } else {
            scrub_value(value);
        }
    }
}

/// Filters the values of sensitive `key=value` pairs, for example `token=[Filtered]`.
pub fn scrub_query(query: &str) -> String {
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) if is_sensitive(key) => format!("{key}={FILTERED}"),
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::scrub_raw_event;

    #[test]
    fn drops_addresses_and_filters_secrets_but_keeps_user_identity() {
        let mut raw = json!({
            "user": {"id": "9", "email": "ada@example.com", "username": "ada", "ip_address": "203.0.113.7"},
            "request": {
                "url": "https://app.example/pay?id=4&access_token=abc",
                "query_string": "page=2&api-key=k1&Token=t",
                "cookies": {"sid": "s"},
                "env": {"REMOTE_ADDR": "203.0.113.7", "SERVER_NAME": "app"},
                "headers": [["Authorization", "Bearer x"], ["Accept", "text/html"], ["Set-Cookie", "a=b"], ["X-Forwarded-For", "203.0.113.7"]],
                "data": {"email": "ada@example.com", "password": "hunter2", "card": {"card_number": "4111"}}
            },
            "extra": {"X-CSRF-Token": "c", "nested": [{"auth": "a", "author": "Ada", "sessionId": "s1"}]},
            "exception": {"values": [{"stacktrace": {"frames": [{"vars": {"secret_value": {"deep": 1}, "count": 3}}]}}]}
        });
        scrub_raw_event(&mut raw);
        assert_eq!(
            raw,
            json!({
                "user": {"id": "9", "email": "ada@example.com", "username": "ada"},
                "request": {
                    "url": "https://app.example/pay?id=4&access_token=[Filtered]",
                    "query_string": "page=2&api-key=[Filtered]&Token=[Filtered]",
                    "env": {"SERVER_NAME": "app"},
                    "headers": [["Authorization", "[Filtered]"], ["Accept", "text/html"], ["Set-Cookie", "[Filtered]"], ["X-Forwarded-For", "[Filtered]"]],
                    "data": {"email": "ada@example.com", "password": "[Filtered]", "card": {"card_number": "[Filtered]"}}
                },
                "extra": {"X-CSRF-Token": "[Filtered]", "nested": [{"auth": "[Filtered]", "author": "Ada", "sessionId": "[Filtered]"}]},
                "exception": {"values": [{"stacktrace": {"frames": [{"vars": {"secret_value": "[Filtered]", "count": 3}}]}}]}
            })
        );
    }

    #[test]
    fn filters_query_string_pairs_and_is_idempotent() {
        let mut raw = json!({"request": {"query_string": [["q", "owl"], ["xsrf", "z"]]}});
        scrub_raw_event(&mut raw);
        let once = raw.clone();
        scrub_raw_event(&mut raw);
        assert_eq!(raw, once);
        assert_eq!(
            raw,
            json!({"request": {"query_string": [["q", "owl"], ["xsrf", "[Filtered]"]]}})
        );
    }
}
