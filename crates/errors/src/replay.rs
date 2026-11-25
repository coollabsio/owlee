//! Session replay items of a Sentry envelope: one `replay_event` and one
//! `replay_recording` make one segment.

use std::fmt;
use std::io::Read;

use flate2::bufread::ZlibDecoder;
use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde_json::Value;
use thiserror::Error;

/// The SDK ends a replay after 60 minutes, about 720 segments of 5 seconds.
pub const MAX_SEGMENT_ID: u16 = 999;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("malformed replay: {0}")]
pub struct ReplayError(pub &'static str);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayType {
    Session,
    Buffer,
}

impl ReplayType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Buffer => "buffer",
        }
    }
}

/// How the recording bytes are stored and served, as an HTTP `Content-Encoding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingEncoding {
    /// zlib (RFC 1950), which HTTP calls `deflate`.
    Deflate,
    Identity,
}

impl RecordingEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deflate => "deflate",
            Self::Identity => "identity",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReplaySegment {
    /// 32 lowercase hex characters.
    pub replay_id: String,
    pub segment_id: u16,
    pub replay_type: ReplayType,
    /// Client clock in Unix milliseconds, not clamped.
    pub timestamp_ms: Option<i64>,
    pub start_ms: Option<i64>,
    pub urls: Vec<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub user_agent: Option<String>,
    /// The whole `replay_event`, for inbound filters and user hashes. Never stored.
    pub event: Value,
    pub encoding: RecordingEncoding,
    /// The recording as the SDK sent it.
    pub data: Vec<u8>,
}

impl ReplaySegment {
    /// `user.username`, else `user.email`, else `user.id`.
    pub fn user_label(&self) -> Option<String> {
        ["username", "email", "id"]
            .iter()
            .find_map(|key| text(self.event.get("user")?.get(*key)?))
    }
}

/// Parses one segment. `max_inflated` bounds the recording's JSON size after
/// inflating. Runs a full JSON scan, so call it off the async runtime.
pub fn parse_replay(
    event: &[u8],
    recording: &[u8],
    max_inflated: usize,
) -> Result<ReplaySegment, ReplayError> {
    let event: Value =
        serde_json::from_slice(event).map_err(|_| ReplayError("replay_event is not JSON"))?;
    let replay_id = event
        .get("replay_id")
        .and_then(Value::as_str)
        .filter(|id| is_replay_id(id))
        .ok_or(ReplayError("replay_id is not 32 lowercase hex characters"))?
        .to_owned();
    let segment_id = segment_id(event.get("segment_id"))?;
    let replay_type = match event.get("replay_type").and_then(Value::as_str) {
        Some("session") => ReplayType::Session,
        Some("buffer") => ReplayType::Buffer,
        _ => return Err(ReplayError("replay_type is not session or buffer")),
    };
    let newline = recording
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or(ReplayError("replay_recording has no header line"))?;
    let header: Value = serde_json::from_slice(&recording[..newline])
        .map_err(|_| ReplayError("replay_recording header is not JSON"))?;
    if segment_id_of(header.get("segment_id")) != Some(segment_id) {
        return Err(ReplayError("segment ids of the two items differ"));
    }
    let data = &recording[newline + 1..];
    let encoding = check_recording(data, max_inflated)?;
    let request = event.get("request");
    let user_agent = request
        .and_then(|request| request.get("headers"))
        .and_then(|headers| {
            headers.as_object()?.iter().find_map(|(name, value)| {
                name.eq_ignore_ascii_case("user-agent")
                    .then(|| text(value))
                    .flatten()
            })
        });
    let urls = event
        .get("urls")
        .and_then(Value::as_array)
        .map(|urls| urls.iter().filter_map(text).collect())
        .unwrap_or_default();
    Ok(ReplaySegment {
        replay_id,
        segment_id,
        replay_type,
        timestamp_ms: seconds_to_ms(event.get("timestamp")),
        start_ms: seconds_to_ms(event.get("replay_start_timestamp")),
        urls,
        release: event.get("release").and_then(text),
        environment: event.get("environment").and_then(text),
        user_agent,
        encoding,
        data: data.to_vec(),
        event,
    })
}

pub fn is_replay_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn segment_id(value: Option<&Value>) -> Result<u16, ReplayError> {
    segment_id_of(value).ok_or(ReplayError("segment_id is not an integer from 0 to 999"))
}

fn segment_id_of(value: Option<&Value>) -> Option<u16> {
    value?
        .as_u64()
        .filter(|id| *id <= u64::from(MAX_SEGMENT_ID))
        .map(|id| id as u16)
}

fn seconds_to_ms(value: Option<&Value>) -> Option<i64> {
    let seconds = value?.as_f64()?;
    (seconds.is_finite() && seconds > 0.0 && seconds < 1e11).then_some((seconds * 1000.0) as i64)
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// zlib or a plain JSON array. Gzip and raw deflate are refused, and the zlib
/// stream must end at the end of the item.
fn check_recording(data: &[u8], max: usize) -> Result<RecordingEncoding, ReplayError> {
    match data {
        [b'[', ..] => {
            if data.len() > max {
                return Err(ReplayError("recording exceeds the size limit"));
            }
            scan_array(data, max)?;
            Ok(RecordingEncoding::Identity)
        }
        [cmf @ 0x78, flg, ..] => {
            let header_ok = (u16::from(*cmf) * 256 + u16::from(*flg)) % 31 == 0;
            // FDICT: a preset dictionary that no browser has.
            if !header_ok || flg & 0x20 != 0 {
                return Err(ReplayError("recording has a bad zlib header"));
            }
            let mut decoder = ZlibDecoder::new(data);
            scan_array(&mut decoder, max)?;
            if !decoder.get_ref().is_empty() {
                return Err(ReplayError("recording has bytes after the zlib stream"));
            }
            Ok(RecordingEncoding::Deflate)
        }
        _ => Err(ReplayError("recording is neither zlib nor a JSON array")),
    }
}

/// Reads the whole input as one JSON array without building it.
fn scan_array(reader: impl Read, max: usize) -> Result<(), ReplayError> {
    let mut limited = reader.take((max as u64).saturating_add(1));
    let mut deserializer = serde_json::Deserializer::from_reader(&mut limited);
    let parsed = ArrayOnly::deserialize(&mut deserializer).and_then(|_| deserializer.end());
    if limited.limit() == 0 {
        return Err(ReplayError("recording exceeds the size limit"));
    }
    parsed.map_err(|error| {
        if error.is_io() {
            ReplayError("recording is not a valid zlib stream")
        } else {
            ReplayError("recording is not a JSON array")
        }
    })
}

struct ArrayOnly;

impl<'de> Deserialize<'de> for ArrayOnly {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Elements;
        impl<'de> Visitor<'de> for Elements {
            type Value = ArrayOnly;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<ArrayOnly, A::Error> {
                while items.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ArrayOnly)
            }
        }
        deserializer.deserialize_seq(Elements)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::{GzEncoder, ZlibEncoder};

    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const EVENTS: &[u8] = br#"[{"type":4,"data":{"href":"https://shop.example/"},"timestamp":1}]"#;

    fn event(segment: u64) -> Vec<u8> {
        serde_json::json!({
            "type": "replay_event",
            "replay_id": ID,
            "replay_type": "buffer",
            "segment_id": segment,
            "timestamp": 1_800_000_005.5,
            "replay_start_timestamp": 1_800_000_000.0,
            "urls": ["https://shop.example/cart?token=abc", 7, ""],
            "release": "shop@1.0",
            "user": { "id": "42", "email": "ada@example.com" },
            "request": { "url": "https://shop.example/", "headers": { "User-Agent": "Firefox/130" } },
        })
        .to_string()
        .into_bytes()
    }

    fn recording(segment: u64, body: &[u8]) -> Vec<u8> {
        let mut bytes = format!("{{\"segment_id\":{segment}}}\n").into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn zlib(body: &[u8], level: Compression) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), level);
        encoder.write_all(body).unwrap();
        encoder.finish().unwrap()
    }

    fn parse(segment: u64, body: &[u8]) -> Result<ReplaySegment, ReplayError> {
        parse_replay(&event(segment), &recording(segment, body), 1024 * 1024)
    }

    #[test]
    fn a_zlib_recording_is_kept_as_sent() {
        let compressed = zlib(EVENTS, Compression::default());
        assert_eq!(&compressed[..2], &[0x78, 0x9c]);
        let segment = parse(3, &compressed).unwrap();
        assert_eq!(segment.replay_id, ID);
        assert_eq!(segment.segment_id, 3);
        assert_eq!(segment.replay_type, ReplayType::Buffer);
        assert_eq!(segment.encoding, RecordingEncoding::Deflate);
        assert_eq!(segment.data, compressed);
        assert_eq!(segment.timestamp_ms, Some(1_800_000_005_500));
        assert_eq!(segment.start_ms, Some(1_800_000_000_000));
        assert_eq!(
            segment.urls,
            vec!["https://shop.example/cart?token=abc".to_owned()]
        );
        assert_eq!(segment.release.as_deref(), Some("shop@1.0"));
        assert_eq!(segment.user_agent.as_deref(), Some("Firefox/130"));
        assert_eq!(segment.user_label().as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn a_plain_recording_is_identity() {
        let segment = parse(0, EVENTS).unwrap();
        assert_eq!(segment.encoding, RecordingEncoding::Identity);
        assert_eq!(segment.data, EVENTS);
    }

    #[test]
    fn other_valid_zlib_headers_pass() {
        for level in [
            Compression::none(),
            Compression::fast(),
            Compression::best(),
        ] {
            let compressed = zlib(EVENTS, level);
            assert_ne!(&compressed[..2], &[0x78, 0x9c]);
            assert_eq!(
                parse(0, &compressed).unwrap().encoding,
                RecordingEncoding::Deflate
            );
        }
    }

    #[test]
    fn a_zlib_header_with_a_bad_check_or_a_dictionary_is_malformed() {
        let mut bad_check = zlib(EVENTS, Compression::default());
        bad_check[1] = 0x9d;
        assert_eq!(
            parse(0, &bad_check).unwrap_err(),
            ReplayError("recording has a bad zlib header")
        );
        // 0x78 0xbb passes the check but sets FDICT.
        let mut dictionary = zlib(EVENTS, Compression::default());
        dictionary[1] = 0xbb;
        assert_eq!(
            parse(0, &dictionary).unwrap_err(),
            ReplayError("recording has a bad zlib header")
        );
    }

    #[test]
    fn gzip_raw_deflate_and_junk_are_malformed() {
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(EVENTS).unwrap();
        let gzip = gzip.finish().unwrap();
        for body in [gzip.as_slice(), b"{\"a\":1}", b"hello", b""] {
            assert_eq!(
                parse(0, body).unwrap_err(),
                ReplayError("recording is neither zlib nor a JSON array"),
                "{body:?}"
            );
        }
    }

    #[test]
    fn mismatched_segment_ids_are_malformed() {
        let result = parse_replay(&event(1), &recording(2, EVENTS), 1024);
        assert_eq!(
            result.unwrap_err(),
            ReplayError("segment ids of the two items differ")
        );
    }

    #[test]
    fn segment_1000_is_malformed() {
        assert_eq!(
            parse(1000, EVENTS).unwrap_err(),
            ReplayError("segment_id is not an integer from 0 to 999")
        );
        assert_eq!(parse(999, EVENTS).unwrap().segment_id, 999);
    }

    #[test]
    fn replay_ids_must_be_lowercase_hex() {
        for id in [
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdeg",
        ] {
            let mut value: Value = serde_json::from_slice(&event(0)).unwrap();
            value["replay_id"] = Value::from(id);
            let result = parse_replay(value.to_string().as_bytes(), &recording(0, EVENTS), 1024);
            assert_eq!(
                result.unwrap_err(),
                ReplayError("replay_id is not 32 lowercase hex characters"),
                "{id}"
            );
        }
    }

    #[test]
    fn bytes_after_the_zlib_stream_are_malformed() {
        let mut compressed = zlib(EVENTS, Compression::default());
        compressed.extend_from_slice(b"\n");
        assert_eq!(
            parse(0, &compressed).unwrap_err(),
            ReplayError("recording has bytes after the zlib stream")
        );
    }

    #[test]
    fn a_truncated_zlib_stream_is_malformed() {
        let compressed = zlib(EVENTS, Compression::default());
        let cut = &compressed[..compressed.len() - 2];
        assert!(parse(0, cut).is_err());
    }

    #[test]
    fn a_recording_that_inflates_past_the_limit_is_malformed() {
        let mut big = b"[\"".to_vec();
        big.extend(std::iter::repeat_n(b'a', 4 * 1024 * 1024));
        big.extend_from_slice(b"\"]");
        let compressed = zlib(&big, Compression::best());
        assert!(compressed.len() < 64 * 1024);
        assert_eq!(
            parse(0, &compressed).unwrap_err(),
            ReplayError("recording exceeds the size limit")
        );
        assert_eq!(
            parse(0, &big).unwrap_err(),
            ReplayError("recording exceeds the size limit")
        );
        let fits = parse_replay(&event(0), &recording(0, &compressed), big.len()).unwrap();
        assert_eq!(fits.encoding, RecordingEncoding::Deflate);
    }

    #[test]
    fn a_recording_that_is_not_a_json_array_is_malformed() {
        for body in [
            zlib(b"{\"type\":4}", Compression::default()),
            zlib(b"[1,2", Compression::default()),
            zlib(b"[1] [2]", Compression::default()),
            b"[1,2".to_vec(),
            b"[1] x".to_vec(),
        ] {
            assert_eq!(
                parse(0, &body).unwrap_err(),
                ReplayError("recording is not a JSON array"),
                "{body:?}"
            );
        }
    }
}
