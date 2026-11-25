//! Telemetry domain: OTLP decoding, log and span normalization, the rollup
//! histogram, and the search language of the logs and traces pages. HTTP and
//! storage live in `apps/server`.

mod decode;
mod logs;
mod search;
mod spans;
mod value;

pub use decode::{
    DecodeError, Encoding, Signal, count_points, decode_logs, decode_traces, export_response,
    status_body,
};
pub use logs::{Level, LogRow, normalize_logs};
pub use opentelemetry_proto::tonic as proto;
pub use prost;
pub use search::{
    Compare, Dataset, Field, MAX_TERMS, Query, QueryError, Term, TermKind, parse_query,
};
pub use spans::{
    BUCKET_BOUNDS_US, BUCKETS, MAX_EVENTS, MAX_LINKS, SpanKind, SpanRow, SpanStatus, bucket_index,
    normalize_spans, operation, percentile_ms,
};
pub use value::{MAX_ATTRIBUTES, MAX_BODY_BYTES, MAX_KEY_CHARS, MAX_VALUE_BYTES};
