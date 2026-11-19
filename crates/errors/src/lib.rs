//! Error-reporting domain: Sentry envelope decoding, event normalization, and
//! issue fingerprinting. HTTP and storage live in `apps/server`.

mod envelope;
mod event;
mod limit;
mod scrub;

pub use envelope::{BodyError, Envelope, EnvelopeError, EnvelopeItem, decode_body, parse_envelope};
pub use event::{NormalizedEvent, normalize_event};
pub use limit::limit_raw_event;
pub use scrub::{scrub_query, scrub_raw_event};
