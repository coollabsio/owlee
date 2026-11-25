//! Telemetry domain: OTLP decoding, log normalization, and the search
//! language of the logs page. HTTP and storage live in `apps/server`.

mod decode;
mod logs;
mod search;
mod value;

pub use decode::{
    DecodeError, Encoding, Signal, count_points, count_spans, decode_logs, export_response,
    status_body,
};
pub use logs::{Level, LogRow, normalize_logs};
pub use opentelemetry_proto::tonic as proto;
pub use prost;
pub use search::{Field, MAX_TERMS, Query, QueryError, Term, TermKind, parse_query};
pub use value::{MAX_ATTRIBUTES, MAX_BODY_BYTES, MAX_KEY_CHARS, MAX_VALUE_BYTES};
