//! Turns captured bodies into something a person can read.

use std::io::Read;

use flate2::read::GzDecoder;

use crate::store::{Captured, Headers};

/// Longest text shown for one body.
pub const DISPLAY_LIMIT: usize = 256 * 1024;
/// Stops a small gzip body from expanding without bound.
const MAX_DECODED: u64 = 16 * 1024 * 1024;

/// A body ready to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyView {
    Empty,
    Json { text: String, truncated: bool },
    Text { text: String, truncated: bool },
    Binary { len: usize },
}

/// A captured body with any gzip encoding removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub bytes: Vec<u8>,
    /// False if `bytes` is only the beginning of the real body.
    pub complete: bool,
}

/// Finds a header, ignoring case.
pub fn header<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Removes gzip encoding. A cut gzip stream yields what could be decoded.
pub fn decode(headers: &Headers, body: &Captured) -> Decoded {
    let gzip = header(headers, "content-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("gzip"));
    if !gzip {
        return Decoded {
            bytes: body.bytes.to_vec(),
            complete: !body.truncated,
        };
    }
    let mut bytes = Vec::new();
    let result = GzDecoder::new(&body.bytes[..])
        .take(MAX_DECODED + 1)
        .read_to_end(&mut bytes);
    if result.is_err() && bytes.is_empty() {
        // Labelled gzip but is not: show it as it is.
        return Decoded {
            bytes: body.bytes.to_vec(),
            complete: !body.truncated,
        };
    }
    let overflowed = bytes.len() as u64 > MAX_DECODED;
    bytes.truncate(MAX_DECODED as usize);
    Decoded {
        bytes,
        complete: result.is_ok() && !overflowed && !body.truncated,
    }
}

/// Prepares a body for display: pretty JSON, plain text, or a binary marker.
pub fn view(headers: &Headers, body: &Captured) -> BodyView {
    let decoded = decode(headers, body);
    if decoded.bytes.is_empty() {
        return BodyView::Empty;
    }
    let looks_like_json = header(headers, "content-type")
        .is_some_and(|value| value.to_ascii_lowercase().contains("json"))
        || matches!(first_significant(&decoded.bytes), Some(b'{' | b'['));
    if looks_like_json {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&decoded.bytes) {
            if let Ok(pretty) = serde_json::to_string_pretty(&value) {
                let (text, cut) = shorten(pretty);
                return BodyView::Json {
                    text,
                    truncated: cut || !decoded.complete,
                };
            }
        }
    }
    let Some(text) = readable_text(&decoded.bytes) else {
        return BodyView::Binary {
            len: decoded.bytes.len(),
        };
    };
    let (text, cut) = shorten(text);
    BodyView::Text {
        text,
        truncated: cut || !decoded.complete,
    }
}

/// True if the decoded body starts as a JSON array.
pub fn is_json_array(bytes: &[u8]) -> bool {
    first_significant(bytes) == Some(b'[')
}

fn first_significant(bytes: &[u8]) -> Option<u8> {
    bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
}

/// Accepts UTF-8, tolerating a character cut at the end of the capture.
fn readable_text(bytes: &[u8]) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text.to_owned()),
        Err(error) if error.error_len().is_none() => {
            let valid = &bytes[..error.valid_up_to()];
            Some(String::from_utf8_lossy(valid).into_owned())
        }
        Err(_) => None,
    }
}

fn shorten(mut text: String) -> (String, bool) {
    if text.len() <= DISPLAY_LIMIT {
        return (text, false);
    }
    let boundary = (0..=DISPLAY_LIMIT)
        .rev()
        .find(|index| text.is_char_boundary(*index))
        .unwrap_or(0);
    text.truncate(boundary);
    (text, true)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use bytes::Bytes;
    use flate2::{write::GzEncoder, Compression};

    use super::*;

    fn gzip(data: &[u8]) -> Bytes {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("write");
        Bytes::from(encoder.finish().expect("finish"))
    }

    fn gzip_headers() -> Headers {
        vec![("Content-Encoding".into(), "gzip".into())]
    }

    #[test]
    fn pretty_prints_json_keeping_key_order() {
        let body = Captured::new(Bytes::from_static(br#"{"z":1,"a":[true]}"#));
        let BodyView::Json { text, truncated } = view(&Vec::new(), &body) else {
            panic!("expected JSON");
        };
        assert!(text.starts_with("{\n  \"z\": 1"), "{text}");
        assert!(!truncated);
    }

    #[test]
    fn decodes_gzip_for_display() {
        let body = Captured::new(gzip(b"[1]"));
        assert!(matches!(
            view(&gzip_headers(), &body),
            BodyView::Json { .. }
        ));
    }

    #[test]
    fn cut_gzip_yields_partial_text() {
        let original = "hello world ".repeat(1000);
        let compressed = gzip(original.as_bytes());
        let cut = Captured {
            bytes: compressed.slice(..compressed.len() / 2),
            truncated: true,
        };
        let decoded = decode(&gzip_headers(), &cut);
        assert!(!decoded.complete);
    }

    #[test]
    fn invalid_gzip_falls_back_to_raw_bytes() {
        let body = Captured::new(Bytes::from_static(b"plain"));
        assert_eq!(decode(&gzip_headers(), &body).bytes, b"plain");
    }

    #[test]
    fn broken_json_is_shown_as_text() {
        let body = Captured::new(Bytes::from_static(b"{not json"));
        assert!(matches!(view(&Vec::new(), &body), BodyView::Text { .. }));
    }

    #[test]
    fn binary_and_empty_bodies() {
        let binary = Captured::new(Bytes::from_static(&[0xff, 0x00, 0x10]));
        assert_eq!(view(&Vec::new(), &binary), BodyView::Binary { len: 3 });
        assert_eq!(view(&Vec::new(), &Captured::default()), BodyView::Empty);
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let body = Captured::new(Bytes::from("я".repeat(DISPLAY_LIMIT)));
        let BodyView::Text { text, truncated } = view(&Vec::new(), &body) else {
            panic!("expected text");
        };
        assert!(truncated);
        assert!(text.len() <= DISPLAY_LIMIT);
    }

    #[test]
    fn detects_json_arrays() {
        assert!(is_json_array(b"  \n[1]"));
        assert!(!is_json_array(b"{}"));
    }
}
