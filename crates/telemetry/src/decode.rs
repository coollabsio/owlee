//! OTLP/HTTP bodies in both encodings the specification allows, and the
//! responses in the encoding of the request.

use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsPartialSuccess, ExportMetricsServiceRequest, ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::metrics::v1::metric::Data;
use prost::Message;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Protobuf,
    Json,
}

impl Encoding {
    /// `None` for a type OTLP/HTTP does not define.
    #[must_use]
    pub fn from_content_type(value: &str) -> Option<Self> {
        let mime = value.split(';').next().unwrap_or("").trim();
        if mime.eq_ignore_ascii_case("application/x-protobuf")
            || mime.eq_ignore_ascii_case("application/protobuf")
        {
            Some(Self::Protobuf)
        } else if mime.eq_ignore_ascii_case("application/json") {
            Some(Self::Json)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Protobuf => "application/x-protobuf",
            Self::Json => "application/json",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("the body is not a valid OTLP protobuf message: {0}")]
    Protobuf(#[from] prost::DecodeError),
    #[error("the body is not valid OTLP JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn decode_logs(
    body: &[u8],
    encoding: Encoding,
) -> Result<ExportLogsServiceRequest, DecodeError> {
    decode(body, encoding)
}

pub fn decode_traces(
    body: &[u8],
    encoding: Encoding,
) -> Result<ExportTraceServiceRequest, DecodeError> {
    decode(body, encoding)
}

/// Data points in an `ExportMetricsServiceRequest`. Owlee does not store metrics yet.
pub fn count_points(body: &[u8], encoding: Encoding) -> Result<u64, DecodeError> {
    let request: ExportMetricsServiceRequest = decode(body, encoding)?;
    Ok(request
        .resource_metrics
        .iter()
        .flat_map(|resource| &resource.scope_metrics)
        .flat_map(|scope| &scope.metrics)
        .map(|metric| match &metric.data {
            Some(Data::Gauge(gauge)) => gauge.data_points.len(),
            Some(Data::Sum(sum)) => sum.data_points.len(),
            Some(Data::Histogram(histogram)) => histogram.data_points.len(),
            Some(Data::ExponentialHistogram(histogram)) => histogram.data_points.len(),
            Some(Data::Summary(summary)) => summary.data_points.len(),
            None => 0,
        } as u64)
        .sum())
}

fn decode<T>(body: &[u8], encoding: Encoding) -> Result<T, DecodeError>
where
    T: Message + Default + serde::de::DeserializeOwned,
{
    match encoding {
        Encoding::Protobuf => Ok(T::decode(body)?),
        Encoding::Json => {
            let mut value: Value = serde_json::from_slice(body)?;
            clear_invalid_ids(&mut value);
            Ok(serde_json::from_value(value)?)
        }
    }
}

/// The OTLP specification says an invalid trace or span id means "no id". The
/// generated serde fails the whole request on one, so it is emptied first.
fn clear_invalid_ids(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if matches!(key.as_str(), "traceId" | "spanId" | "parentSpanId") {
                    let valid = value
                        .as_str()
                        .is_some_and(|id| id.len() % 2 == 0 && hex::decode(id).is_ok());
                    if !valid {
                        *value = Value::String(String::new());
                    }
                } else {
                    clear_invalid_ids(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(clear_invalid_ids),
        _ => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Logs,
    Traces,
    Metrics,
}

/// `Export<Signal>ServiceResponse`, with `partial_success` when `rejected` is above 0.
#[must_use]
pub fn export_response(
    signal: Signal,
    encoding: Encoding,
    rejected: u64,
    message: &str,
) -> Vec<u8> {
    let rejected_i64 = i64::try_from(rejected).unwrap_or(i64::MAX);
    let message = message.to_owned();
    match encoding {
        Encoding::Json => {
            if rejected == 0 {
                return b"{}".to_vec();
            }
            let field = match signal {
                Signal::Logs => "rejectedLogRecords",
                Signal::Traces => "rejectedSpans",
                Signal::Metrics => "rejectedDataPoints",
            };
            // int64 is a decimal string in OTLP/JSON.
            json!({ "partialSuccess": { field: rejected.to_string(), "errorMessage": message } })
                .to_string()
                .into_bytes()
        }
        Encoding::Protobuf => match signal {
            Signal::Logs => ExportLogsServiceResponse {
                partial_success: (rejected > 0).then_some(ExportLogsPartialSuccess {
                    rejected_log_records: rejected_i64,
                    error_message: message,
                }),
            }
            .encode_to_vec(),
            Signal::Traces => ExportTraceServiceResponse {
                partial_success: (rejected > 0).then_some(ExportTracePartialSuccess {
                    rejected_spans: rejected_i64,
                    error_message: message,
                }),
            }
            .encode_to_vec(),
            Signal::Metrics => ExportMetricsServiceResponse {
                partial_success: (rejected > 0).then_some(ExportMetricsPartialSuccess {
                    rejected_data_points: rejected_i64,
                    error_message: message,
                }),
            }
            .encode_to_vec(),
        },
    }
}

/// `google.rpc.Status`, the OTLP error body.
#[derive(Clone, PartialEq, Message)]
struct Status {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}

/// An error body. `code` is a gRPC status code, such as 3 for a bad argument.
#[must_use]
pub fn status_body(encoding: Encoding, code: i32, message: &str) -> Vec<u8> {
    match encoding {
        Encoding::Json => json!({ "code": code, "message": message })
            .to_string()
            .into_bytes(),
        Encoding::Protobuf => Status {
            code,
            message: message.to_owned(),
        }
        .encode_to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(
            Encoding::from_content_type("application/x-protobuf"),
            Some(Encoding::Protobuf)
        );
        assert_eq!(
            Encoding::from_content_type("Application/JSON; charset=utf-8"),
            Some(Encoding::Json)
        );
        assert_eq!(Encoding::from_content_type("text/plain"), None);
    }

    #[test]
    fn counts_points_and_decodes_spans_in_json() {
        let traces = br#"{"resourceSpans":[{"scopeSpans":[{"spans":[
            {"traceId":"5b8efff798038103d269b633813fc60c","spanId":"eee19b7ec3c1b174","name":"a"},
            {"traceId":"not hex","spanId":"","name":"b"}]}]}]}"#;
        let decoded = decode_traces(traces, Encoding::Json).unwrap();
        assert_eq!(decoded.resource_spans[0].scope_spans[0].spans.len(), 2);
        assert!(
            decoded.resource_spans[0].scope_spans[0].spans[1]
                .trace_id
                .is_empty()
        );
        let metrics = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
            {"name":"a","gauge":{"dataPoints":[{"asDouble":1.5},{"asInt":"2"}]}},
            {"name":"b","sum":{"dataPoints":[{"asInt":3}],"isMonotonic":true,"aggregationTemporality":2}}]}]}]}"#;
        assert_eq!(count_points(metrics, Encoding::Json).unwrap(), 3);
        assert!(matches!(
            decode_traces(b"{", Encoding::Json),
            Err(DecodeError::Json(_))
        ));
        assert!(matches!(
            decode_traces(&[0xff, 0xff, 0xff], Encoding::Protobuf),
            Err(DecodeError::Protobuf(_))
        ));
    }

    #[test]
    fn responses_in_both_encodings() {
        assert_eq!(
            export_response(Signal::Logs, Encoding::Json, 0, ""),
            b"{}".to_vec()
        );
        assert_eq!(
            String::from_utf8(export_response(
                Signal::Traces,
                Encoding::Json,
                4,
                "traces are not stored yet"
            ))
            .unwrap(),
            r#"{"partialSuccess":{"errorMessage":"traces are not stored yet","rejectedSpans":"4"}}"#
        );
        let bytes = export_response(Signal::Logs, Encoding::Protobuf, 2, "daily quota");
        let decoded = ExportLogsServiceResponse::decode(bytes.as_slice()).unwrap();
        assert_eq!(
            decoded.partial_success,
            Some(ExportLogsPartialSuccess {
                rejected_log_records: 2,
                error_message: "daily quota".into()
            })
        );
        assert_eq!(
            String::from_utf8(status_body(Encoding::Json, 3, "bad")).unwrap(),
            r#"{"code":3,"message":"bad"}"#
        );
    }
}
