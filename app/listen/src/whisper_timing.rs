//! v1.8.2 token byte fragments are not individually UTF-8. Deserialize those
//! fields as bytes and join complete code points before assigning overlap
//! ownership. A malformed/incomplete result is recoverable failure, never a
//! lossy transcription. The same builder receives the full JSON from the CLI
//! and the timed tokens from the in-process model host, so both backends share
//! one ownership rule.
use crate::{AppResult, TimedText};
use serde::{
    Deserialize, Deserializer,
    de::{Error, Visitor},
};
use std::{fmt, fs, path::Path};

#[derive(Deserialize)]
struct Document {
    model: Model,
    transcription: Vec<Segment>,
}
#[derive(Deserialize)]
struct Model {
    multilingual: bool,
}
#[derive(Deserialize)]
struct Segment {
    text: String,
    tokens: Vec<Token>,
}
#[derive(Deserialize)]
struct Token {
    id: u32,
    #[serde(deserialize_with = "token_bytes")]
    text: Vec<u8>,
    offsets: Option<Offsets>,
}
#[derive(Deserialize)]
struct Offsets {
    from: i64,
    to: i64,
}

/// One decoded token as the application sees it: the original bytes, the token
/// id and the optional millisecond span. The CLI JSON and the model host both
/// produce this shape.
pub(crate) struct TimedToken {
    pub id: u32,
    pub text: Vec<u8>,
    pub offsets: Option<(i64, i64)>,
}

fn token_bytes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct Bytes;
    impl<'de> Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("whisper token bytes")
        }
        fn visit_bytes<E: Error>(self, value: &[u8]) -> Result<Vec<u8>, E> {
            Ok(value.to_vec())
        }
        fn visit_borrowed_bytes<E: Error>(self, value: &'de [u8]) -> Result<Vec<u8>, E> {
            Ok(value.to_vec())
        }
    }
    deserializer.deserialize_bytes(Bytes)
}

pub(crate) fn read(path: &Path) -> AppResult<Vec<TimedText>> {
    let bytes =
        fs::read(path).map_err(|e| format!("Local backend did not produce timed JSON: {e}"))?;
    let document: Document = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Local backend produced incomplete timed JSON: {e}"))?;
    let segments = document.transcription.into_iter().map(|segment| {
        let tokens = segment
            .tokens
            .into_iter()
            .map(|token| TimedToken {
                id: token.id,
                text: token.text,
                offsets: token.offsets.map(|offsets| (offsets.from, offsets.to)),
            })
            .collect();
        (segment.text, tokens)
    });
    build(document.model.multilingual, segments)
}

/// Build the shareable timed spans from complete segments. Segment text must
/// match the joined token text; otherwise the backend result is incomplete and
/// the caller must treat the audio as retained.
pub(crate) fn build(
    multilingual: bool,
    segments: impl IntoIterator<Item = (String, Vec<TimedToken>)>,
) -> AppResult<Vec<TimedText>> {
    // These are Whisper's fixed end-of-text IDs; language/control/timestamp
    // tokens all follow them. Real text is preserved even if it looks special.
    let end_of_text = if multilingual { 50_257 } else { 50_256 };
    let mut result: Vec<TimedText> = Vec::new();
    for (segment_text, tokens) in segments {
        let mut bytes = Vec::new();
        let mut begin = None;
        let mut rendered = String::new();
        for token in tokens.into_iter().filter(|t| t.id < end_of_text) {
            if token.text.is_empty() {
                continue;
            }
            let (from, to) = token
                .offsets
                .ok_or("Local backend omitted text token timestamps; audio retained")?;
            if from < 0 || to < from {
                return Err(
                    "Local backend returned invalid text token timestamps; audio retained".into(),
                );
            }
            let start = *begin.get_or_insert(from as u64);
            bytes.extend(token.text);
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    rendered.push_str(text);
                    // Keep Latin subword tokens together so a split inside a word
                    // cannot select different halves from adjacent windows.
                    let continuation = result.last().is_some_and(|previous| {
                        previous.text.ends_with(|c: char| c.is_ascii_alphanumeric())
                            && text.starts_with(|c: char| c.is_ascii_alphanumeric())
                    });
                    if continuation {
                        let previous = result.last_mut().unwrap();
                        previous.text.push_str(text);
                        previous.end_ms = to as u64;
                    } else {
                        result.push(TimedText {
                            start_ms: start,
                            end_ms: to as u64,
                            text: text.into(),
                        });
                    }
                    bytes.clear();
                    begin = None;
                }
                Err(error) if error.error_len().is_none() => {}
                Err(_) => {
                    return Err(
                        "Local backend returned invalid UTF-8 text tokens; audio retained".into(),
                    );
                }
            }
        }
        if !bytes.is_empty() || rendered.trim() != segment_text.trim() {
            return Err(
                "Local timed result does not contain the complete decoded text; audio retained"
                    .into(),
            );
        }
    }
    Ok(result)
}
