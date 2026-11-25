//! Error-reporting domain: Sentry envelope decoding, event normalization, and
//! issue fingerprinting. HTTP and storage live in `apps/server`.

mod envelope;
mod event;
mod limit;
mod replay;
mod scrub;

pub use envelope::{BodyError, Envelope, EnvelopeError, EnvelopeItem, decode_body, parse_envelope};
pub use event::{NormalizedEvent, normalize_event};
pub use limit::limit_raw_event;
pub use replay::{
    MAX_SEGMENT_ID, RecordingEncoding, ReplayError, ReplaySegment, ReplayType, is_replay_id,
    parse_replay,
};
pub use scrub::{scrub_query, scrub_raw_event, scrub_text};
