//! Removes secrets and client addresses from a Sentry event before it is stored.
//!
//! Values under a sensitive key are replaced. Every other string is searched
//! for secrets that do not need a key to be found: bearer and basic
//! credentials, JWTs, AWS access key ids, PEM private keys, and payment card
//! numbers that pass the Luhn check.
//!
//! `user.id`, `user.email`, and `user.username` stay. The operator chooses to
//! send them through the SDK, and privacy requests find events by them.

use std::borrow::Cow;

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

pub fn is_sensitive(key: &str) -> bool {
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
        Value::String(text) => {
            if let Cow::Owned(scrubbed) = scrub_text(text) {
                *text = scrubbed;
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

/// Replaces secrets found by their shape, for example `Bearer [Filtered]`.
pub fn scrub_text(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        let at_word = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let found = private_key(bytes, i).map(|end| (i, end)).or_else(|| {
            if !at_word {
                return None;
            }
            jwt(bytes, i)
                .or_else(|| aws_key(bytes, i))
                .or_else(|| card(bytes, i))
                .map(|end| (i, end))
                .or_else(|| auth_scheme(bytes, i))
        });
        match found {
            Some((start, end)) => {
                out.push_str(&text[copied..start]);
                out.push_str(FILTERED);
                copied = end;
                i = end;
            }
            None => i += 1,
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

fn starts_with_ignore_case(bytes: &[u8], at: usize, prefix: &[u8]) -> bool {
    bytes
        .get(at..at + prefix.len())
        .is_some_and(|slice| slice.eq_ignore_ascii_case(prefix))
}

fn run(bytes: &[u8], from: usize, allowed: impl Fn(u8) -> bool) -> usize {
    bytes[from..]
        .iter()
        .position(|byte| !allowed(*byte))
        .map_or(bytes.len(), |offset| from + offset)
}

fn ends_word(bytes: &[u8], end: usize) -> bool {
    bytes.get(end).is_none_or(|byte| !byte.is_ascii_alphanumeric())
}

/// A whole `-----BEGIN ... PRIVATE KEY-----` block, up to its END line.
fn private_key(bytes: &[u8], at: usize) -> Option<usize> {
    const BEGIN: &[u8] = b"-----BEGIN ";
    if !bytes[at..].starts_with(BEGIN) {
        return None;
    }
    let line_end = at + BEGIN.len() + find(&bytes[at + BEGIN.len()..], b"-----")?;
    if !bytes[at..line_end].ends_with(b"PRIVATE KEY") {
        return None;
    }
    let end_marker = line_end + find(&bytes[line_end..], b"-----END ")?;
    let close = end_marker + 9 + find(&bytes[end_marker + 9..], b"-----")?;
    Some(close + 5)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Three base64url segments, the first a JSON header (`eyJ`).
fn jwt(bytes: &[u8], at: usize) -> Option<usize> {
    if !bytes[at..].starts_with(b"eyJ") {
        return None;
    }
    let base64url = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_';
    let mut end = at;
    for segment in 0..3 {
        let next = run(bytes, end, base64url);
        if next - end < 4 {
            return None;
        }
        end = next;
        if segment < 2 {
            if bytes.get(end) != Some(&b'.') {
                return None;
            }
            end += 1;
        }
    }
    Some(end)
}

fn aws_key(bytes: &[u8], at: usize) -> Option<usize> {
    if !(bytes[at..].starts_with(b"AKIA") || bytes[at..].starts_with(b"ASIA")) {
        return None;
    }
    let end = run(bytes, at + 4, |byte| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit()
    });
    (end - at == 20 && ends_word(bytes, end)).then_some(end)
}

/// 13 to 19 digits, with single spaces or dashes between them, that pass the
/// Luhn check. The first digit is 2 to 6, so millisecond timestamps, which
/// start with 1, are not cards.
fn card(bytes: &[u8], at: usize) -> Option<usize> {
    if !(b'2'..=b'6').contains(&bytes[at]) {
        return None;
    }
    let mut digits = Vec::new();
    let mut end = at;
    let mut i = at;
    while i < bytes.len() && digits.len() < 19 {
        match bytes[i] {
            byte @ b'0'..=b'9' => {
                digits.push(byte - b'0');
                i += 1;
                end = i;
            }
            b' ' | b'-' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) && end == i => i += 1,
            _ => break,
        }
    }
    ((13..=19).contains(&digits.len()) && ends_word(bytes, end) && luhn(&digits)).then_some(end)
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(index, &digit)| {
            let digit = u32::from(digit);
            if index % 2 == 1 {
                let doubled = digit * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                digit
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// `Bearer <token>` or `Basic <credentials>`. The word stays and the value is
/// filtered. A value of only lowercase letters is prose, as in "basic support".
fn auth_scheme(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let scheme = [b"bearer ".as_slice(), b"basic ".as_slice()]
        .into_iter()
        .find(|scheme| starts_with_ignore_case(bytes, at, scheme))?;
    let start = at + scheme.len();
    let end = run(bytes, start, |byte| {
        byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte)
    });
    let token = &bytes[start..end];
    (token.len() >= 8 && !token.iter().all(u8::is_ascii_lowercase)).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{scrub_raw_event, scrub_text};

    #[test]
    fn filters_secrets_found_by_shape_in_any_string() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlLXZhbHVl";
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK\n-----END RSA PRIVATE KEY-----";
        let mut raw = json!({
            "message": format!("call failed with Authorization: Bearer sk_live_9fA2b7Qx and {jwt}."),
            "breadcrumbs": {"values": [
                {"message": "charge card 4111 1111 1111 1111 for order 42"},
                {"message": "key AKIAIOSFODNN7EXAMPLE loaded"},
                {"message": format!("config {pem} end")},
                {"message": "basic support plan, at 1700000000000, card-like 4111111111111112"}
            ]}
        });
        scrub_raw_event(&mut raw);
        assert_eq!(
            raw,
            json!({
                "message": "call failed with Authorization: Bearer [Filtered] and [Filtered].",
                "breadcrumbs": {"values": [
                    {"message": "charge card [Filtered] for order 42"},
                    {"message": "key [Filtered] loaded"},
                    {"message": "config [Filtered] end"},
                    {"message": "basic support plan, at 1700000000000, card-like 4111111111111112"}
                ]}
            })
        );
        assert_eq!(scrub_text("Basic dXNlcjpwYXNz"), "Basic [Filtered]");
        assert_eq!(scrub_text("4111-1111-1111-1111"), "[Filtered]");
    }

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
