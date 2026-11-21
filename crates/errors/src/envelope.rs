use std::io::Read;

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BodyError {
    #[error("unsupported content encoding: {0}")]
    Encoding(String),
    #[error("failed to decompress body: {0}")]
    Decompress(String),
    #[error("decoded body exceeds the size limit")]
    TooLarge,
}

#[derive(Debug, Error)]
pub enum EnvelopeError {
    #[error("envelope header is not valid JSON")]
    Header,
    #[error("envelope item header is not valid JSON")]
    ItemHeader,
    #[error("envelope item length exceeds the body")]
    Truncated,
}

#[derive(Debug, Clone)]
pub struct Envelope {
    pub header: Value,
    pub items: Vec<EnvelopeItem>,
}

#[derive(Debug, Clone)]
pub struct EnvelopeItem {
    pub item_type: String,
    pub header: Value,
    pub payload: Vec<u8>,
}

/// Decodes a request body to at most `max` bytes. A body that inflates past
/// `max` is refused without being buffered.
pub fn decode_body(
    content_encoding: Option<&str>,
    body: &[u8],
    max: usize,
) -> Result<Vec<u8>, BodyError> {
    let encoding = content_encoding
        .unwrap_or("")
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match encoding.as_str() {
        "" if body.starts_with(&[0x1f, 0x8b]) => gunzip(body, max),
        "" if body.len() > max => Err(BodyError::TooLarge),
        "" => Ok(body.to_vec()),
        "gzip" | "x-gzip" => gunzip(body, max),
        "deflate" => inflate(body, max),
        other => Err(BodyError::Encoding(other.to_owned())),
    }
}

fn gunzip(body: &[u8], max: usize) -> Result<Vec<u8>, BodyError> {
    read_limited(GzDecoder::new(body), max)
}

fn inflate(body: &[u8], max: usize) -> Result<Vec<u8>, BodyError> {
    match read_limited(ZlibDecoder::new(body), max) {
        Err(BodyError::Decompress(_)) => read_limited(DeflateDecoder::new(body), max),
        result => result,
    }
}

fn read_limited(reader: impl Read, max: usize) -> Result<Vec<u8>, BodyError> {
    let mut output = Vec::new();
    reader
        .take((max as u64).saturating_add(1))
        .read_to_end(&mut output)
        .map_err(|error| BodyError::Decompress(error.to_string()))?;
    if output.len() > max {
        return Err(BodyError::TooLarge);
    }
    Ok(output)
}

/// Parse a Sentry envelope. Unknown item types are returned so the caller can ignore them.
pub fn parse_envelope(bytes: &[u8]) -> Result<Envelope, EnvelopeError> {
    let mut index = 0;
    let header_line = read_line(bytes, &mut index);
    let header: Value = if header_line.is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_slice(&header_line).map_err(|_| EnvelopeError::Header)?
    };
    let mut items = Vec::new();
    while index < bytes.len() {
        if bytes[index] == b'\n' || bytes[index] == b'\r' {
            index += 1;
            continue;
        }
        let header_line = read_line(bytes, &mut index);
        if header_line.is_empty() {
            continue;
        }
        let item_header: Value =
            serde_json::from_slice(&header_line).map_err(|_| EnvelopeError::ItemHeader)?;
        let item_type = item_header
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let payload = if let Some(length) = json_usize(item_header.get("length")) {
            let end = index
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(EnvelopeError::Truncated)?;
            let payload = bytes[index..end].to_vec();
            index = end;
            if index < bytes.len() && (bytes[index] == b'\n' || bytes[index] == b'\r') {
                if bytes[index] == b'\r' && index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                    index += 2;
                } else {
                    index += 1;
                }
            }
            payload
        } else {
            read_line(bytes, &mut index)
        };
        items.push(EnvelopeItem {
            item_type,
            header: item_header,
            payload,
        });
    }
    Ok(Envelope { header, items })
}

fn json_usize(value: Option<&Value>) -> Option<usize> {
    let value = value?;
    value.as_u64().map(|number| number as usize).or_else(|| {
        value
            .as_i64()
            .filter(|number| *number >= 0)
            .map(|number| number as usize)
    })
}

fn read_line(bytes: &[u8], index: &mut usize) -> Vec<u8> {
    if *index >= bytes.len() {
        return Vec::new();
    }
    let start = *index;
    while *index < bytes.len() && bytes[*index] != b'\n' {
        *index += 1;
    }
    let mut end = *index;
    if *index < bytes.len() && bytes[*index] == b'\n' {
        *index += 1;
    }
    if end > start && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    bytes[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    #[test]
    fn parses_event_and_ignorable_items() {
        let event = br#"{"event_id":"abc","message":"hello"}"#;
        let session = br#"{"sid":"s"}"#;
        let mut body = Vec::new();
        body.extend_from_slice(br#"{"event_id":"abc","dsn":"http://public@localhost/1"}"#);
        body.push(b'\n');
        let event_header = format!(r#"{{"type":"event","length":{}}}"#, event.len());
        body.extend_from_slice(event_header.as_bytes());
        body.push(b'\n');
        body.extend_from_slice(event);
        body.push(b'\n');
        body.extend_from_slice(br#"{"type":"session"}"#);
        body.push(b'\n');
        body.extend_from_slice(session);
        body.push(b'\n');
        body.extend_from_slice(br#"{"type":"client_report","length":2}"#);
        body.push(b'\n');
        body.extend_from_slice(br#"{}"#);
        body.push(b'\n');

        let envelope = parse_envelope(&body).unwrap();
        assert_eq!(envelope.items.len(), 3);
        assert_eq!(envelope.items[0].item_type, "event");
        assert_eq!(envelope.items[0].payload, event);
        assert_eq!(envelope.items[1].item_type, "session");
        assert_eq!(envelope.items[2].item_type, "client_report");
        assert_eq!(
            envelope.header.get("dsn").and_then(Value::as_str),
            Some("http://public@localhost/1")
        );
    }

    #[test]
    fn gunzips_envelope_bodies() {
        let raw = b"{\"event_id\":\"abc\"}\n";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).unwrap();
        let gzipped = encoder.finish().unwrap();
        let decoded = decode_body(Some("gzip"), &gzipped, 1024).unwrap();
        assert_eq!(decoded, raw);
        let sniffed = decode_body(None, &gzipped, 1024).unwrap();
        assert_eq!(sniffed, raw);
    }

    #[test]
    fn refuses_a_body_that_inflates_past_the_limit() {
        let bomb = vec![b'a'; 4 * 1024 * 1024];
        let mut gzip = GzEncoder::new(Vec::new(), Compression::best());
        gzip.write_all(&bomb).unwrap();
        let gzipped = gzip.finish().unwrap();
        assert!(gzipped.len() < 8 * 1024, "{}", gzipped.len());
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), Compression::best());
        zlib.write_all(&bomb).unwrap();
        let deflated = zlib.finish().unwrap();

        let max = 1024 * 1024;
        for (encoding, body) in [
            (Some("gzip"), &gzipped),
            (None, &gzipped),
            (Some("deflate"), &deflated),
        ] {
            let refused = decode_body(encoding, body, max);
            assert!(
                matches!(refused, Err(BodyError::TooLarge)),
                "{encoding:?}: {refused:?}"
            );
        }
        assert!(matches!(
            decode_body(None, &bomb, max),
            Err(BodyError::TooLarge)
        ));
        assert_eq!(
            decode_body(Some("gzip"), &gzipped, bomb.len()).unwrap(),
            bomb
        );
    }

    #[test]
    fn item_length_that_overflows_is_truncated_not_a_panic() {
        let body = format!("{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{{}}", u64::MAX);
        assert!(matches!(
            parse_envelope(body.as_bytes()),
            Err(EnvelopeError::Truncated)
        ));
    }
}
