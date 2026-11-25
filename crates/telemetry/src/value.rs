//! OTLP attribute values as JSON and as filterable key-value pairs, cut to the
//! limits of the design and scrubbed of secrets and client addresses.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use opentelemetry_proto::tonic::common::v1::any_value::Value as Any;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};
use owlee_errors::{is_sensitive, scrub_text};
use serde_json::{Map, Value};

pub const MAX_ATTRIBUTES: usize = 64;
pub const MAX_KEY_CHARS: usize = 128;
pub const MAX_VALUE_BYTES: usize = 1024;
pub const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_DEPTH: usize = 8;
const FILTERED: &str = "[Filtered]";
/// Client addresses, removed as `user.ip_address` is for errors.
const REMOVED: &[&str] = &["client.address", "source.address", "net.peer.ip"];

pub(crate) struct Attributes {
    pub json: Map<String, Value>,
    /// String, number, and bool values, for the key-value table.
    pub pairs: Vec<(String, String)>,
}

pub(crate) fn attributes(list: &[KeyValue]) -> Attributes {
    let mut json = Map::new();
    let mut pairs = Vec::new();
    for pair in list
        .iter()
        .filter(|pair| !REMOVED.contains(&pair.key.as_str()))
    {
        if json.len() == MAX_ATTRIBUTES {
            break;
        }
        let key = cut_chars(&pair.key, MAX_KEY_CHARS);
        if json.contains_key(&key) {
            continue;
        }
        let value = if is_sensitive(&pair.key) {
            Value::String(FILTERED.to_owned())
        } else {
            to_json(pair.value.as_ref(), 0)
        };
        if let Some(text) = scalar_text(&value) {
            pairs.push((key.clone(), text));
        }
        json.insert(key, value);
    }
    Attributes { json, pairs }
}

/// A string body as it is. Any other body as JSON text.
pub(crate) fn body_text(body: Option<&AnyValue>) -> String {
    let text = match body.and_then(|body| body.value.as_ref()) {
        None => String::new(),
        Some(Any::StringValue(text)) => scrub_text(text).into_owned(),
        Some(_) => to_json(body, 0).to_string(),
    };
    cut_bytes(&text, MAX_BODY_BYTES).to_owned()
}

/// The first string value of `key`, unscrubbed: service and host names.
pub(crate) fn string_attribute<'a>(list: &'a [KeyValue], key: &str) -> Option<&'a str> {
    list.iter().find(|pair| pair.key == key).and_then(|pair| {
        match pair.value.as_ref()?.value.as_ref()? {
            Any::StringValue(text) if !text.is_empty() => Some(text.as_str()),
            _ => None,
        }
    })
}

fn to_json(value: Option<&AnyValue>, depth: usize) -> Value {
    let Some(value) = value.and_then(|value| value.value.as_ref()) else {
        return Value::Null;
    };
    match value {
        Any::StringValue(text) => Value::String(clip(&scrub_text(text))),
        Any::BoolValue(flag) => Value::Bool(*flag),
        Any::IntValue(number) => Value::from(*number),
        Any::DoubleValue(number) => {
            serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number)
        }
        Any::BytesValue(bytes) => Value::String(clip(&STANDARD.encode(bytes))),
        Any::ArrayValue(array) if depth < MAX_DEPTH => Value::Array(
            array
                .values
                .iter()
                .take(MAX_ATTRIBUTES)
                .map(|item| to_json(Some(item), depth + 1))
                .collect(),
        ),
        Any::KvlistValue(list) if depth < MAX_DEPTH => {
            let mut map = Map::new();
            for pair in list
                .values
                .iter()
                .filter(|pair| !REMOVED.contains(&pair.key.as_str()))
                .take(MAX_ATTRIBUTES)
            {
                let value = if is_sensitive(&pair.key) {
                    Value::String(FILTERED.to_owned())
                } else {
                    to_json(pair.value.as_ref(), depth + 1)
                };
                map.insert(cut_chars(&pair.key, MAX_KEY_CHARS), value);
            }
            Value::Object(map)
        }
        Any::ArrayValue(_) | Any::KvlistValue(_) | Any::StringValueStrindex(_) => Value::Null,
    }
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn clip(text: &str) -> String {
    cut_bytes(text, MAX_VALUE_BYTES).to_owned()
}

pub(crate) fn cut_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(crate) fn cut_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
