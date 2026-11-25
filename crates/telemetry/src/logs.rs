//! `ExportLogsServiceRequest` to log rows.

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::KeyValue;

use crate::value::{self, MAX_KEY_CHARS, MAX_VALUE_BYTES};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub const ALL: [Self; 6] = [
        Self::Trace,
        Self::Debug,
        Self::Info,
        Self::Warn,
        Self::Error,
        Self::Fatal,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
        }
    }

    /// The stored names only, as the search language takes them.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == text)
    }

    /// `severity_number` ranges of the OTLP log data model, else the text.
    fn from_severity(number: i32, text: &str) -> Self {
        match number {
            1..=4 => Self::Trace,
            5..=8 => Self::Debug,
            9..=12 => Self::Info,
            13..=16 => Self::Warn,
            17..=20 => Self::Error,
            21..=24 => Self::Fatal,
            _ => match text.trim().to_ascii_lowercase().as_str() {
                "trace" => Self::Trace,
                "debug" => Self::Debug,
                "warn" | "warning" => Self::Warn,
                "error" | "err" => Self::Error,
                "fatal" | "critical" | "crit" | "emerg" | "emergency" | "alert" | "panic" => {
                    Self::Fatal
                }
                _ => Self::Info,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogRow {
    pub time_ms: i64,
    pub level: Level,
    pub severity_text: String,
    pub service: String,
    pub environment: String,
    pub host: String,
    pub body: String,
    /// 32 lowercase hex characters, or `None` when absent or invalid.
    pub trace_id: Option<String>,
    /// 16 lowercase hex characters, or `None` when absent or invalid.
    pub span_id: Option<String>,
    /// String, number, and bool attributes, for the key-value table.
    pub attributes: Vec<(String, String)>,
    pub attributes_json: String,
    pub resource_json: String,
}

struct Resource {
    service: String,
    environment: String,
    host: String,
    json: String,
}

impl Resource {
    fn of(attributes: &[KeyValue]) -> Self {
        let text = |key: &str| {
            value::string_attribute(attributes, key)
                .map(|text| value::cut_bytes(text, MAX_VALUE_BYTES).to_owned())
        };
        Self {
            service: text("service.name").unwrap_or_else(|| "unknown_service".to_owned()),
            environment: text("deployment.environment.name")
                .or_else(|| text("deployment.environment"))
                .unwrap_or_default(),
            host: text("host.name").unwrap_or_default(),
            json: serde_json::Value::Object(value::attributes(attributes).json).to_string(),
        }
    }
}

/// One row for each log record. A record is never dropped: what is above a
/// limit is cut. `received_ms` is the time of a record with no timestamp.
#[must_use]
pub fn normalize_logs(request: &ExportLogsServiceRequest, received_ms: i64) -> Vec<LogRow> {
    let mut rows = Vec::new();
    for resource_logs in &request.resource_logs {
        let resource = Resource::of(
            resource_logs
                .resource
                .as_ref()
                .map_or(&[][..], |resource| &resource.attributes),
        );
        for record in resource_logs
            .scope_logs
            .iter()
            .flat_map(|scope| &scope.log_records)
        {
            let attributes = value::attributes(&record.attributes);
            let nanos = if record.time_unix_nano > 0 {
                record.time_unix_nano
            } else {
                record.observed_time_unix_nano
            };
            rows.push(LogRow {
                time_ms: if nanos > 0 {
                    i64::try_from(nanos / 1_000_000).unwrap_or(received_ms)
                } else {
                    received_ms
                },
                level: Level::from_severity(record.severity_number, &record.severity_text),
                severity_text: value::cut_chars(&record.severity_text, MAX_KEY_CHARS),
                service: resource.service.clone(),
                environment: resource.environment.clone(),
                host: resource.host.clone(),
                body: value::body_text(record.body.as_ref()),
                trace_id: hex_id(&record.trace_id, 16),
                span_id: hex_id(&record.span_id, 8),
                attributes: attributes.pairs,
                attributes_json: serde_json::Value::Object(attributes.json).to_string(),
                resource_json: resource.json.clone(),
            });
        }
    }
    rows
}

/// An id of the wrong length or of all zeroes is invalid in OTLP.
fn hex_id(bytes: &[u8], length: usize) -> Option<String> {
    (bytes.len() == length && bytes.iter().any(|byte| *byte != 0)).then(|| hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::common::v1::any_value::Value as Any;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, ArrayValue, KeyValue, KeyValueList};
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use prost::Message;

    use super::*;
    use crate::{Encoding, decode_logs};

    const RECEIVED: i64 = 1_700_000_000_000;

    fn text(value: &str) -> Option<AnyValue> {
        Some(AnyValue {
            value: Some(Any::StringValue(value.to_owned())),
        })
    }

    fn pair(key: &str, value: Option<AnyValue>) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value,
            ..KeyValue::default()
        }
    }

    fn any(value: Any) -> Option<AnyValue> {
        Some(AnyValue { value: Some(value) })
    }

    fn request(resource: Vec<KeyValue>, records: Vec<LogRecord>) -> ExportLogsServiceRequest {
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: resource,
                    ..Resource::default()
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: records,
                    ..ScopeLogs::default()
                }],
                ..ResourceLogs::default()
            }],
        }
    }

    /// Normalizes the request after a round trip through protobuf bytes, as intake does.
    fn from_protobuf(request: &ExportLogsServiceRequest) -> Vec<LogRow> {
        let bytes = request.encode_to_vec();
        normalize_logs(&decode_logs(&bytes, Encoding::Protobuf).unwrap(), RECEIVED)
    }

    #[test]
    fn a_protobuf_record_becomes_one_row() {
        let rows = from_protobuf(&request(
            vec![
                pair("service.name", text("checkout")),
                pair("deployment.environment.name", text("production")),
                pair("deployment.environment", text("ignored")),
                pair("host.name", text("web-1")),
                pair("client.address", text("203.0.113.9")),
            ],
            vec![LogRecord {
                time_unix_nano: 1_700_000_123_456_789_000,
                observed_time_unix_nano: 1_700_000_999_000_000_000,
                severity_number: 17,
                severity_text: "ERROR".into(),
                body: text("payment failed for order 42"),
                attributes: vec![
                    pair("http.route", text("/checkout")),
                    pair("http.status_code", any(Any::IntValue(502))),
                    pair("retry", any(Any::BoolValue(false))),
                    pair("ratio", any(Any::DoubleValue(0.25))),
                    pair(
                        "tags",
                        any(Any::ArrayValue(ArrayValue {
                            values: vec![text("a").unwrap(), text("b").unwrap()],
                        })),
                    ),
                    pair("net.peer.ip", text("10.0.0.1")),
                    pair("api_key", text("sk_live_123")),
                ],
                trace_id: (1..=16).collect(),
                span_id: vec![0xab; 8],
                ..LogRecord::default()
            }],
        ));
        assert_eq!(
            rows,
            vec![LogRow {
                time_ms: 1_700_000_123_456,
                level: Level::Error,
                severity_text: "ERROR".into(),
                service: "checkout".into(),
                environment: "production".into(),
                host: "web-1".into(),
                body: "payment failed for order 42".into(),
                trace_id: Some("0102030405060708090a0b0c0d0e0f10".into()),
                span_id: Some("abababababababab".into()),
                attributes: vec![
                    ("http.route".into(), "/checkout".into()),
                    ("http.status_code".into(), "502".into()),
                    ("retry".into(), "false".into()),
                    ("ratio".into(), "0.25".into()),
                    ("api_key".into(), "[Filtered]".into()),
                ],
                attributes_json: r#"{"api_key":"[Filtered]","http.route":"/checkout","http.status_code":502,"ratio":0.25,"retry":false,"tags":["a","b"]}"#.into(),
                resource_json: r#"{"deployment.environment":"ignored","deployment.environment.name":"production","host.name":"web-1","service.name":"checkout"}"#.into(),
            }]
        );
    }

    #[test]
    fn a_json_request_from_the_otlp_example_becomes_rows() {
        // The logs example of opentelemetry-proto `examples/logs.json`, with a second record.
        let body = br#"{
          "resourceLogs": [{
            "resource": { "attributes": [
              { "key": "service.name", "value": { "stringValue": "my.service" } },
              { "key": "deployment.environment", "value": { "stringValue": "staging" } }
            ] },
            "scopeLogs": [{
              "scope": { "name": "my.library", "version": "1.0.0" },
              "logRecords": [{
                "timeUnixNano": "1544712660300000000",
                "observedTimeUnixNano": "1544712660300000000",
                "severityNumber": 10,
                "severityText": "Information",
                "traceId": "5B8EFFF798038103D269B633813FC60C",
                "spanId": "EEE19B7EC3C1B174",
                "body": { "stringValue": "Example log record" },
                "attributes": [
                  { "key": "string.attribute", "value": { "stringValue": "some string" } },
                  { "key": "boolean.attribute", "value": { "boolValue": true } },
                  { "key": "int.attribute", "value": { "intValue": "10" } },
                  { "key": "double.attribute", "value": { "doubleValue": 637.704 } },
                  { "key": "array.attribute", "value": { "arrayValue": { "values": [
                    { "stringValue": "many" }, { "stringValue": "values" } ] } } },
                  { "key": "map.attribute", "value": { "kvlistValue": { "values": [
                    { "key": "some.map.key", "value": { "stringValue": "some value" } },
                    { "key": "password", "value": { "stringValue": "hunter2" } } ] } } }
                ]
              }, {
                "observedTimeUnixNano": 1544712661000000000,
                "severityText": "warning",
                "traceId": "zz",
                "spanId": "",
                "body": { "kvlistValue": { "values": [
                  { "key": "msg", "value": { "stringValue": "Authorization: Bearer abc.def.ghi" } } ] } }
              }]
            }]
          }]
        }"#;
        let rows = normalize_logs(&decode_logs(body, Encoding::Json).unwrap(), RECEIVED);
        let resource = r#"{"deployment.environment":"staging","service.name":"my.service"}"#;
        assert_eq!(
            rows,
            vec![
                LogRow {
                    time_ms: 1_544_712_660_300,
                    level: Level::Info,
                    severity_text: "Information".into(),
                    service: "my.service".into(),
                    environment: "staging".into(),
                    host: String::new(),
                    body: "Example log record".into(),
                    trace_id: Some("5b8efff798038103d269b633813fc60c".into()),
                    span_id: Some("eee19b7ec3c1b174".into()),
                    attributes: vec![
                        ("string.attribute".into(), "some string".into()),
                        ("boolean.attribute".into(), "true".into()),
                        ("int.attribute".into(), "10".into()),
                        ("double.attribute".into(), "637.704".into()),
                    ],
                    attributes_json: r#"{"array.attribute":["many","values"],"boolean.attribute":true,"double.attribute":637.704,"int.attribute":10,"map.attribute":{"password":"[Filtered]","some.map.key":"some value"},"string.attribute":"some string"}"#.into(),
                    resource_json: resource.into(),
                },
                LogRow {
                    time_ms: 1_544_712_661_000,
                    level: Level::Warn,
                    severity_text: "warning".into(),
                    service: "my.service".into(),
                    environment: "staging".into(),
                    host: String::new(),
                    body: r#"{"msg":"Authorization: Bearer [Filtered]"}"#.into(),
                    trace_id: None,
                    span_id: None,
                    attributes: vec![],
                    attributes_json: "{}".into(),
                    resource_json: resource.into(),
                },
            ]
        );
    }

    #[test]
    fn severity_numbers_and_texts() {
        let cases = [
            (1, "", Level::Trace),
            (4, "", Level::Trace),
            (5, "", Level::Debug),
            (9, "", Level::Info),
            (13, "", Level::Warn),
            (16, "", Level::Warn),
            (17, "", Level::Error),
            (21, "", Level::Fatal),
            (24, "", Level::Fatal),
            (0, "WARN", Level::Warn),
            (0, "err", Level::Error),
            (0, "CRITICAL", Level::Fatal),
            (0, "debug", Level::Debug),
            (0, "", Level::Info),
            (0, "verbose", Level::Info),
            (9, "error", Level::Info),
        ];
        for (number, text, expected) in cases {
            assert_eq!(
                Level::from_severity(number, text),
                expected,
                "{number} {text}"
            );
        }
    }

    #[test]
    fn fallbacks_for_time_service_and_ids() {
        let rows = from_protobuf(&request(
            vec![],
            vec![LogRecord {
                trace_id: vec![0; 16],
                span_id: vec![1; 4],
                ..LogRecord::default()
            }],
        ));
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.time_ms, RECEIVED);
        assert_eq!(row.service, "unknown_service");
        assert_eq!(row.environment, "");
        assert_eq!(row.trace_id, None);
        assert_eq!(row.span_id, None);
        assert_eq!(row.body, "");
        assert_eq!(row.resource_json, "{}");
    }

    #[test]
    fn limits_cut_attributes_keys_values_and_bodies() {
        let mut attributes: Vec<KeyValue> = (0..70)
            .map(|index| pair(&format!("k{index:02}"), text("v")))
            .collect();
        attributes.insert(0, pair(&"x".repeat(200), text(&"é".repeat(600))));
        let rows = from_protobuf(&request(
            vec![pair("service.name", text("api"))],
            vec![LogRecord {
                body: text(&"b".repeat(20_000)),
                attributes,
                ..LogRecord::default()
            }],
        ));
        let row = &rows[0];
        assert_eq!(row.body.len(), 16 * 1024);
        assert_eq!(row.attributes.len(), 64);
        assert_eq!(row.attributes[0].0, "x".repeat(128));
        assert_eq!(row.attributes[0].1, "é".repeat(512));
        assert_eq!(row.attributes[63].0, "k62");
    }

    #[test]
    fn nested_values_are_scrubbed() {
        let rows = from_protobuf(&request(
            vec![],
            vec![LogRecord {
                body: any(Any::KvlistValue(KeyValueList {
                    values: vec![
                        pair("session", text("s3cr3t")),
                        pair("source.address", text("198.51.100.4")),
                        pair("n", any(Any::IntValue(-3))),
                    ],
                })),
                attributes: vec![pair("note", text("card 4111 1111 1111 1111 used"))],
                ..LogRecord::default()
            }],
        ));
        assert_eq!(rows[0].body, r#"{"n":-3,"session":"[Filtered]"}"#);
        assert_eq!(
            rows[0].attributes,
            vec![("note".to_owned(), "card [Filtered] used".to_owned())]
        );
    }
}
