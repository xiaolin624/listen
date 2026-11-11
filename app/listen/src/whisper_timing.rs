//! v1.8.2 full JSON writes token byte fragments, which are not individually
//! UTF-8. Deserialize those fields as bytes and join complete code points before
//! assigning overlap ownership. A malformed/incomplete result is recoverable
//! failure, never a lossy transcription.
use crate::{AppResult, TimedText};
use serde::{Deserialize, Deserializer, de::{Error, Visitor}};
use std::{fmt, fs, path::Path};

#[derive(Deserialize)]
struct Document { model: Model, transcription: Vec<Segment> }
#[derive(Deserialize)]
struct Model { multilingual: bool }
#[derive(Deserialize)]
struct Segment { text: String, tokens: Vec<Token> }
#[derive(Deserialize)]
struct Token {
    id: u32,
    #[serde(deserialize_with = "token_bytes")]
    text: Vec<u8>,
    offsets: Option<Offsets>,
}
#[derive(Deserialize)]
struct Offsets { from: i64, to: i64 }

fn token_bytes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct Bytes;
    impl<'de> Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result { formatter.write_str("whisper token bytes") }
        fn visit_bytes<E: Error>(self, value: &[u8]) -> Result<Vec<u8>, E> { Ok(value.to_vec()) }
        fn visit_borrowed_bytes<E: Error>(self, value: &'de [u8]) -> Result<Vec<u8>, E> { Ok(value.to_vec()) }
    }
    deserializer.deserialize_bytes(Bytes)
}

pub(crate) fn read(path: &Path) -> AppResult<Vec<TimedText>> {
    let bytes = fs::read(path).map_err(|e| format!("Local backend did not produce timed JSON: {e}"))?;
    let document: Document = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Local backend produced incomplete timed JSON: {e}"))?;
    // These are Whisper's fixed end-of-text IDs; language/control/timestamp
    // tokens all follow them. Real text is preserved even if it looks special.
    let end_of_text = if document.model.multilingual { 50_257 } else { 50_256 };
    let mut result: Vec<TimedText> = Vec::new();
    for segment in document.transcription {
        let mut bytes = Vec::new();
        let mut begin = None;
        let mut rendered = String::new();
        for token in segment.tokens.into_iter().filter(|t| t.id < end_of_text) {
            if token.text.is_empty() { continue; }
            let offsets = token.offsets.ok_or("Local backend omitted text token timestamps; audio retained")?;
            if offsets.from < 0 || offsets.to < offsets.from {
                return Err("Local backend returned invalid text token timestamps; audio retained".into());
            }
            let start = *begin.get_or_insert(offsets.from as u64);
            bytes.extend(token.text);
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    rendered.push_str(text);
                    // Keep Latin subword tokens together so a split inside a word
                    // cannot select different halves from adjacent windows.
                    let continuation = result.last().is_some_and(|previous|
                        previous.text.ends_with(|c: char| c.is_ascii_alphanumeric())
                        && text.starts_with(|c: char| c.is_ascii_alphanumeric()));
                    if continuation {
                        let previous = result.last_mut().unwrap();
                        previous.text.push_str(text);
                        previous.end_ms = offsets.to as u64;
                    } else {
                        result.push(TimedText { start_ms: start, end_ms: offsets.to as u64, text: text.into() });
                    }
                    bytes.clear();
                    begin = None;
                }
                Err(error) if error.error_len().is_none() => {},
                Err(_) => return Err("Local backend returned invalid UTF-8 text tokens; audio retained".into()),
            }
        }
        if !bytes.is_empty() || rendered.trim() != segment.text.trim() {
            return Err("Local timed result does not contain the complete decoded text; audio retained".into());
        }
    }
    Ok(result)
}
