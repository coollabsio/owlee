//! `ExportTraceServiceRequest` to span rows, and the latency histogram of the
//! service rollup.

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::Span;
use owlee_errors::scrub_text;
use serde_json::{Value, json};

use crate::logs::{Resource, hex_id};
use crate::value::{self, MAX_VALUE_BYTES};

/// Events and links kept for each span.
pub const MAX_EVENTS: usize = 32;
pub const MAX_LINKS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl SpanKind {
    pub const ALL: [Self; 5] = [
        Self::Internal,
        Self::Server,
        Self::Client,
        Self::Producer,
        Self::Consumer,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Server => "server",
            Self::Client => "client",
            Self::Producer => "producer",
            Self::Consumer => "consumer",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// The OTLP enum. Unspecified is internal, as the specification allows.
    const fn from_otlp(number: i32) -> Self {
        match number {
            2 => Self::Server,
            3 => Self::Client,
            4 => Self::Producer,
            5 => Self::Consumer,
            _ => Self::Internal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanStatus {
    Unset,
    Ok,
    Error,
}

impl SpanStatus {
    pub const ALL: [Self; 3] = [Self::Unset, Self::Ok, Self::Error];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == text)
    }

    const fn from_otlp(code: i32) -> Self {
        match code {
            1 => Self::Ok,
            2 => Self::Error,
            _ => Self::Unset,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpanRow {
    /// 32 lowercase hex characters.
    pub trace_id: String,
    /// 16 lowercase hex characters.
    pub span_id: String,
    /// Empty for a root span.
    pub parent_span_id: String,
    pub service: String,
    pub environment: String,
    pub host: String,
    pub name: String,
    pub kind: SpanKind,
    pub start_us: i64,
    pub duration_us: i64,
    pub status: SpanStatus,
    pub status_message: String,
    /// A root span, or a server or consumer span: one request into the service.
    pub is_entry: bool,
    pub http_method: Option<String>,
    pub http_route: Option<String>,
    pub http_status: Option<i64>,
    /// String, number, and bool attributes, for the key-value table.
    pub attributes: Vec<(String, String)>,
    pub attributes_json: String,
    /// `[{"name", "timeMs", "attributes"}]`, as the trace page shows them.
    pub events_json: String,
    /// `[{"traceId", "spanId", "attributes"}]`.
    pub links_json: String,
    pub resource_json: String,
}

impl SpanRow {
    #[must_use]
    pub fn operation(&self) -> String {
        operation(
            &self.name,
            self.http_method.as_deref(),
            self.http_route.as_deref(),
        )
    }
}

/// `GET /checkout` from the method and route, the route alone, else the span name.
#[must_use]
pub fn operation(name: &str, method: Option<&str>, route: Option<&str>) -> String {
    match (method, route) {
        (Some(method), Some(route)) => format!("{method} {route}"),
        (None, Some(route)) => route.to_owned(),
        _ => name.to_owned(),
    }
}

/// Span rows, and how many spans were dropped for an invalid trace or span id.
/// Nothing else drops a span: what is above a limit is cut.
#[must_use]
pub fn normalize_spans(
    request: &ExportTraceServiceRequest,
    received_ms: i64,
) -> (Vec<SpanRow>, u64) {
    let mut rows = Vec::new();
    let mut invalid = 0;
    for resource_spans in &request.resource_spans {
        let resource = Resource::of(
            resource_spans
                .resource
                .as_ref()
                .map_or(&[][..], |resource| &resource.attributes),
        );
        for span in resource_spans
            .scope_spans
            .iter()
            .flat_map(|scope| &scope.spans)
        {
            match span_row(span, &resource, received_ms) {
                Some(row) => rows.push(row),
                None => invalid += 1,
            }
        }
    }
    (rows, invalid)
}

fn span_row(span: &Span, resource: &Resource, received_ms: i64) -> Option<SpanRow> {
    let trace_id = hex_id(&span.trace_id, 16)?;
    let span_id = hex_id(&span.span_id, 8)?;
    let parent_span_id = hex_id(&span.parent_span_id, 8).unwrap_or_default();
    let attributes = value::attributes(&span.attributes);
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| attributes.json.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
    };
    let http_method = text(&["http.request.method", "http.method"]);
    let http_route = text(&["http.route"]);
    let http_status = ["http.response.status_code", "http.status_code"]
        .iter()
        .find_map(|key| match attributes.json.get(*key)? {
            Value::Number(number) => number.as_i64(),
            Value::String(text) => text.parse().ok(),
            _ => None,
        });
    let kind = SpanKind::from_otlp(span.kind);
    let start_us = if span.start_time_unix_nano > 0 {
        i64::try_from(span.start_time_unix_nano / 1000).unwrap_or(received_ms * 1000)
    } else {
        received_ms * 1000
    };
    let duration_us = i64::try_from(
        span.end_time_unix_nano
            .saturating_sub(span.start_time_unix_nano)
            / 1000,
    )
    .unwrap_or(i64::MAX);
    let status = span.status.as_ref();
    let events: Vec<Value> = span
        .events
        .iter()
        .take(MAX_EVENTS)
        .map(|event| {
            json!({
                "name": scrubbed(&event.name),
                "timeMs": (event.time_unix_nano / 1000) as f64 / 1000.0,
                "attributes": Value::Object(value::attributes(&event.attributes).json),
            })
        })
        .collect();
    let links: Vec<Value> = span
        .links
        .iter()
        .take(MAX_LINKS)
        .filter_map(|link| {
            Some(json!({
                "traceId": hex_id(&link.trace_id, 16)?,
                "spanId": hex_id(&link.span_id, 8)?,
                "attributes": Value::Object(value::attributes(&link.attributes).json),
            }))
        })
        .collect();
    Some(SpanRow {
        is_entry: parent_span_id.is_empty()
            || matches!(kind, SpanKind::Server | SpanKind::Consumer),
        trace_id,
        span_id,
        parent_span_id,
        service: resource.service.clone(),
        environment: resource.environment.clone(),
        host: resource.host.clone(),
        name: scrubbed(&span.name),
        kind,
        start_us,
        duration_us,
        status: SpanStatus::from_otlp(status.map_or(0, |status| status.code)),
        status_message: scrubbed(status.map_or("", |status| status.message.as_str())),
        http_method,
        http_route,
        http_status,
        attributes: attributes.pairs,
        attributes_json: Value::Object(attributes.json).to_string(),
        events_json: Value::Array(events).to_string(),
        links_json: Value::Array(links).to_string(),
        resource_json: resource.json.clone(),
    })
}

fn scrubbed(text: &str) -> String {
    value::cut_bytes(&scrub_text(text), MAX_VALUE_BYTES).to_owned()
}

/// Upper bounds of the rollup buckets `b00` to `b18`, in microseconds. `b19` is above the last.
pub const BUCKET_BOUNDS_US: [i64; 19] = [
    1_000,
    2_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_500_000,
    5_000_000,
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
];
pub const BUCKETS: usize = BUCKET_BOUNDS_US.len() + 1;

/// The bucket of a duration. A bound belongs to the bucket below it.
#[must_use]
pub fn bucket_index(duration_us: i64) -> usize {
    BUCKET_BOUNDS_US
        .iter()
        .position(|bound| duration_us <= *bound)
        .unwrap_or(BUCKETS - 1)
}

/// The `quantile` (0 to 1) of a histogram, in milliseconds, or 0 for an empty one.
/// The value is interpolated linearly inside its bucket, from 0 for the first bucket.
/// The last bucket has no upper bound, so a value in it is its lower bound, 1000 s.
#[must_use]
pub fn percentile_ms(buckets: &[i64; BUCKETS], quantile: f64) -> f64 {
    let total: i64 = buckets.iter().sum();
    if total <= 0 {
        return 0.0;
    }
    let rank = quantile.clamp(0.0, 1.0) * total as f64;
    let mut below = 0.0;
    for (index, count) in buckets.iter().enumerate() {
        let count = *count as f64;
        if count > 0.0 && below + count >= rank {
            let lower = if index == 0 {
                0.0
            } else {
                BUCKET_BOUNDS_US[index - 1] as f64
            };
            let Some(upper) = BUCKET_BOUNDS_US.get(index) else {
                return lower / 1000.0;
            };
            let fraction = ((rank - below) / count).clamp(0.0, 1.0);
            return (lower + (*upper as f64 - lower) * fraction) / 1000.0;
        }
        below += count;
    }
    0.0
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::common::v1::any_value::Value as Any;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};
    use opentelemetry_proto::tonic::resource::v1::Resource as OtlpResource;
    use opentelemetry_proto::tonic::trace::v1::span::{Event, Link};
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Status};
    use prost::Message;

    use super::*;
    use crate::{Encoding, decode_traces};

    const RECEIVED: i64 = 1_700_000_000_000;

    fn pair(key: &str, value: Any) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue { value: Some(value) }),
            ..KeyValue::default()
        }
    }

    fn text(value: &str) -> Any {
        Any::StringValue(value.to_owned())
    }

    fn request(resource: Vec<KeyValue>, spans: Vec<Span>) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(OtlpResource {
                    attributes: resource,
                    ..OtlpResource::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans,
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        }
    }

    fn from_protobuf(request: &ExportTraceServiceRequest) -> (Vec<SpanRow>, u64) {
        let bytes = request.encode_to_vec();
        normalize_spans(
            &decode_traces(&bytes, Encoding::Protobuf).unwrap(),
            RECEIVED,
        )
    }

    #[test]
    fn a_protobuf_server_span_becomes_one_row() {
        let (rows, invalid) = from_protobuf(&request(
            vec![
                pair("service.name", text("checkout")),
                pair("deployment.environment", text("production")),
                pair("host.name", text("web-1")),
            ],
            vec![Span {
                trace_id: (1..=16).collect(),
                span_id: vec![0xab; 8],
                parent_span_id: vec![0xcd; 8],
                name: "POST /checkout".into(),
                kind: 2,
                start_time_unix_nano: 1_700_000_000_123_456_789,
                end_time_unix_nano: 1_700_000_000_135_456_789,
                attributes: vec![
                    pair("http.request.method", text("POST")),
                    pair("http.route", text("/checkout")),
                    pair("http.response.status_code", Any::IntValue(502)),
                    pair("client.address", text("203.0.113.9")),
                    pair("password", text("hunter2")),
                ],
                events: vec![Event {
                    time_unix_nano: 1_700_000_000_130_000_000,
                    name: "exception".into(),
                    attributes: vec![pair("exception.message", text("Bearer abc.def.ghi"))],
                    ..Event::default()
                }],
                links: vec![
                    Link {
                        trace_id: vec![0x11; 16],
                        span_id: vec![0x22; 8],
                        ..Link::default()
                    },
                    Link::default(),
                ],
                status: Some(Status {
                    message: "upstream failed".into(),
                    code: 2,
                }),
                ..Span::default()
            }],
        ));
        assert_eq!(invalid, 0);
        assert_eq!(
            rows,
            vec![SpanRow {
                trace_id: "0102030405060708090a0b0c0d0e0f10".into(),
                span_id: "abababababababab".into(),
                parent_span_id: "cdcdcdcdcdcdcdcd".into(),
                service: "checkout".into(),
                environment: "production".into(),
                host: "web-1".into(),
                name: "POST /checkout".into(),
                kind: SpanKind::Server,
                start_us: 1_700_000_000_123_456,
                duration_us: 12_000,
                status: SpanStatus::Error,
                status_message: "upstream failed".into(),
                is_entry: true,
                http_method: Some("POST".into()),
                http_route: Some("/checkout".into()),
                http_status: Some(502),
                attributes: vec![
                    ("http.request.method".into(), "POST".into()),
                    ("http.route".into(), "/checkout".into()),
                    ("http.response.status_code".into(), "502".into()),
                    ("password".into(), "[Filtered]".into()),
                ],
                attributes_json: r#"{"http.request.method":"POST","http.response.status_code":502,"http.route":"/checkout","password":"[Filtered]"}"#.into(),
                events_json: r#"[{"attributes":{"exception.message":"Bearer [Filtered]"},"name":"exception","timeMs":1700000000130.0}]"#.into(),
                links_json: r#"[{"attributes":{},"spanId":"2222222222222222","traceId":"11111111111111111111111111111111"}]"#.into(),
                resource_json: r#"{"deployment.environment":"production","host.name":"web-1","service.name":"checkout"}"#.into(),
            }]
        );
        assert_eq!(rows[0].operation(), "POST /checkout");
    }

    #[test]
    fn a_json_request_with_old_http_keys() {
        let body = br#"{"resourceSpans":[{"resource":{"attributes":[
            {"key":"service.name","value":{"stringValue":"api"}}]},
          "scopeSpans":[{"spans":[
            {"traceId":"5B8EFFF798038103D269B633813FC60C","spanId":"EEE19B7EC3C1B174","name":"handler",
             "kind":3,"startTimeUnixNano":"1544712660000000000","endTimeUnixNano":"1544712660500000000",
             "attributes":[{"key":"http.method","value":{"stringValue":"GET"}},
                           {"key":"http.status_code","value":{"stringValue":"404"}}]},
            {"traceId":"5b8efff798038103d269b633813fc60c","spanId":"eee19b7ec3c1b175",
             "parentSpanId":"eee19b7ec3c1b174","name":"SELECT users","kind":3,
             "startTimeUnixNano":"1544712660100000000","endTimeUnixNano":"1544712660000000000"},
            {"traceId":"","spanId":"eee19b7ec3c1b176","name":"lost"},
            {"traceId":"5b8efff798038103d269b633813fc60c","spanId":"zz","name":"bad id"}
          ]}]}]}"#;
        let (rows, invalid) =
            normalize_spans(&decode_traces(body, Encoding::Json).unwrap(), RECEIVED);
        assert_eq!(invalid, 2);
        let summary: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.span_id.as_str(),
                    row.parent_span_id.as_str(),
                    row.kind,
                    row.start_us,
                    row.duration_us,
                    row.is_entry,
                    row.http_method.as_deref(),
                    row.http_status,
                    row.operation(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "eee19b7ec3c1b174",
                    "",
                    SpanKind::Client,
                    1_544_712_660_000_000,
                    500_000,
                    true,
                    Some("GET"),
                    Some(404),
                    "handler".to_owned()
                ),
                (
                    "eee19b7ec3c1b175",
                    "eee19b7ec3c1b174",
                    SpanKind::Client,
                    1_544_712_660_100_000,
                    0,
                    false,
                    None,
                    None,
                    "SELECT users".to_owned()
                ),
            ]
        );
        assert_eq!(rows[0].trace_id, "5b8efff798038103d269b633813fc60c");
        assert_eq!(rows[0].status, SpanStatus::Unset);
        assert_eq!(rows[0].events_json, "[]");
    }

    #[test]
    fn operations_and_defaults() {
        assert_eq!(operation("x", Some("GET"), Some("/a")), "GET /a");
        assert_eq!(operation("x", None, Some("/a")), "/a");
        assert_eq!(operation("x", Some("GET"), None), "x");
        let (rows, _) = from_protobuf(&request(
            vec![],
            vec![Span {
                trace_id: vec![1; 16],
                span_id: vec![2; 8],
                parent_span_id: vec![0; 8],
                events: (0..40)
                    .map(|index| Event {
                        name: format!("e{index}"),
                        ..Event::default()
                    })
                    .collect(),
                attributes: (0..70)
                    .map(|index| pair(&format!("k{index:02}"), Any::BoolValue(true)))
                    .collect(),
                ..Span::default()
            }],
        ));
        let row = &rows[0];
        assert_eq!(row.service, "unknown_service");
        assert_eq!(row.kind, SpanKind::Internal);
        assert_eq!(row.parent_span_id, "");
        assert!(row.is_entry);
        assert_eq!(row.start_us, RECEIVED * 1000);
        assert_eq!(row.attributes.len(), 64);
        let events: Vec<Value> = serde_json::from_str(&row.events_json).unwrap();
        assert_eq!(events.len(), 32);
    }

    #[test]
    fn buckets_and_percentiles() {
        assert_eq!(bucket_index(0), 0);
        assert_eq!(bucket_index(1_000), 0);
        assert_eq!(bucket_index(1_001), 1);
        assert_eq!(bucket_index(500_000), 8);
        assert_eq!(bucket_index(1_000_000_000), 18);
        assert_eq!(bucket_index(1_000_000_001), 19);

        let mut buckets = [0; BUCKETS];
        assert_eq!(percentile_ms(&buckets, 0.5), 0.0);
        // 10 spans in 5-10 ms, 10 in 50-100 ms.
        buckets[3] = 10;
        buckets[6] = 10;
        assert_eq!(percentile_ms(&buckets, 0.5), 10.0);
        assert_eq!(percentile_ms(&buckets, 0.25), 7.5);
        assert_eq!(percentile_ms(&buckets, 0.95), 95.0);
        assert_eq!(percentile_ms(&buckets, 1.0), 100.0);
        let mut slow = [0; BUCKETS];
        slow[19] = 1;
        assert_eq!(percentile_ms(&slow, 0.99), 1_000_000.0);
    }
}
