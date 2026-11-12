//! The optional DeepSeek refine client (精修).
//!
//! This is the product's only outbound-network path, so the module is split in two:
//! pure request/response functions that unit tests pin down exactly, and one WinHTTP
//! POST that carries nothing but the request body built by [`request_body`].
//!
//! Privacy boundary:
//! * the request body contains the model id, the fixed system prompt and one
//!   transcript string; no audio, file path, runtime directory, endpoint name,
//!   process id, machine name or second transcript is ever included;
//! * the API key comes from `DEEPSEEK_API_KEY` and nowhere else, is only used in the
//!   `Authorization` header, and is never logged, formatted into an error or written
//!   into `status.txt`, `panel.txt` or any evidence file;
//! * a completion whose `finish_reason` is not `stop` is a failure, not a partial
//!   replacement, so a truncated correction can never be applied.

use crate::{AppResult, RefineText};
use serde_json::{Value, json};
use std::{
    env,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::GetLastError,
        Networking::WinHttp::{
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
            WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
            WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData,
            WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
        },
    },
    core::PCWSTR,
};

/// The verified endpoint (see the DeepSeek chat-completions reference).
const ENDPOINT_HOST: &str = "api.deepseek.com";
const ENDPOINT_PATH: &str = "/chat/completions";
/// The current fast model id. The old `deepseek-chat` id is no longer listed.
const MODEL: &str = "deepseek-flash";
/// Thinking mode is on by default; a deterministic correction must turn it off.
const THINKING: &str = "disabled";
/// A bounded completion: a one-utterance correction fits comfortably, and a runaway
/// answer fails with `finish_reason=length` instead of being applied.
const MAX_TOKENS: u32 = 4096;
/// A response larger than this is not a correction of one utterance.
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const RESOLVE_TIMEOUT_MS: i32 = 5_000;
const CONNECT_TIMEOUT_MS: i32 = 5_000;
const SEND_TIMEOUT_MS: i32 = 10_000;
const RECEIVE_TIMEOUT_MS: i32 = 30_000;
/// A total bound in addition to the per-operation WinHTTP timeouts, so a server that
/// trickles bytes cannot hold a worker forever.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);

/// The one instruction sent with every transcript. It asks for the smallest change
/// that fixes obvious recognition mistakes and forbids any rewrite of the content.
pub const SYSTEM_PROMPT: &str = "You correct speech-to-text transcripts. The user message is one transcript produced by a local speech recognizer. Correct only obvious recognition mistakes: proper nouns, technical terms, homophones and punctuation. Keep the meaning, the wording, the speaker's language mixing (for example Chinese with English technical terms) and every number exactly as intended. Do not translate, summarize, expand, shorten or add commentary. Output only the corrected transcript text: no explanations, no quotes, no code fences. If nothing needs correcting, return the transcript unchanged.";

/// The client read from the local environment. Construction never touches the
/// network; only an explicit [`RefineText::refine`] call does.
pub struct DeepSeek {
    /// The key is kept as an opaque string. It is only ever put into the
    /// `Authorization` header; nothing in this module formats it into a message.
    key: Option<String>,
}

impl DeepSeek {
    /// Read the key from `DEEPSEEK_API_KEY` only. An empty or whitespace-only value
    /// counts as "no key", so refine stays unavailable instead of sending a request
    /// that can only fail.
    pub fn from_environment() -> Self {
        Self {
            key: normalize_key(env::var("DEEPSEEK_API_KEY").ok()),
        }
    }

    /// The key, or the explicit availability reason. The reason names the variable so
    /// the user can act, and never contains the value.
    fn key(&self) -> Result<&str, String> {
        self.key.as_deref().ok_or_else(|| {
            "No DeepSeek API key is configured. Set DEEPSEEK_API_KEY in the local environment to enable refine; local recognition, delivery and the current result are unaffected."
                .to_string()
        })
    }
}

/// Trim the raw environment value and treat blank as absent. Kept separate from the
/// environment read so the normalization rule is unit-testable.
fn normalize_key(raw: Option<String>) -> Option<String> {
    raw.map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

impl RefineText for DeepSeek {
    fn availability(&self) -> Result<(), String> {
        self.key().map(|_| ())
    }

    fn refine(&self, text: &str) -> AppResult<String> {
        let key = self.key()?;
        if text.trim().is_empty() {
            return Err("There is no complete result to refine".into());
        }
        let body = request_body(text);
        let (status, response) = post_json(key, &body)?;
        parse_response(status, &response)
    }
}

/// The exact request body. `messages[1].content` is the only user data in it.
pub fn request_body(text: &str) -> String {
    json!({
        "model": MODEL,
        "thinking": {"type": THINKING},
        "temperature": 0,
        "stream": false,
        "max_tokens": MAX_TOKENS,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": text},
        ],
    })
    .to_string()
}

/// Turn one HTTP response into the corrected text or a failure. Nothing is returned
/// for a non-200 status, a malformed body, a non-`stop` finish reason, or empty text:
/// every one of those would otherwise be a partial replacement.
pub fn parse_response(status: u16, body: &str) -> AppResult<String> {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    if status != 200 {
        let detail = parsed
            .as_ref()
            .and_then(|value| value.pointer("/error/message"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(|message| format!(": {message}"))
            .unwrap_or_else(|| {
                let clipped: String = body.trim().chars().take(200).collect();
                if clipped.is_empty() {
                    String::new()
                } else {
                    format!(": {clipped}")
                }
            });
        return Err(format!("DeepSeek API returned HTTP {status}{detail}"));
    }
    let value = parsed.ok_or_else(|| "The DeepSeek API response is not valid JSON".to_string())?;
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| {
            "The DeepSeek API response did not contain a completion choice".to_string()
        })?;
    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .ok_or_else(|| "The DeepSeek API response did not report a finish reason".to_string())?;
    if finish != "stop" {
        return Err(format!(
            "The DeepSeek refine stopped before a complete answer (finish_reason={finish}); the correction was not applied"
        ));
    }
    let content = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| "The DeepSeek API response did not contain corrected text".to_string())?;
    let content = content.trim();
    if content.is_empty() {
        return Err(
            "The DeepSeek API returned an empty correction; the local result is kept".into(),
        );
    }
    Ok(content.to_string())
}

/// One WinHTTP handle that is always closed, in reverse creation order.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(raw: *mut core::ffi::c_void, what: &str) -> AppResult<Self> {
        if raw.is_null() {
            Err(format!("{what}: {}", last_error()))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn last_error() -> String {
    let code = unsafe { GetLastError() };
    format!("Windows error {}", code.0)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

/// POST the body over HTTPS and return `(status, utf8 body)`. The only credential use
/// is the `Authorization` header built here.
fn post_json(key: &str, body: &str) -> AppResult<(u16, String)> {
    let agent = wide("Listen");
    let session = unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    };
    let session = Handle::new(session, "Cannot start the DeepSeek HTTPS session")?;
    unsafe {
        WinHttpSetTimeouts(
            session.0,
            RESOLVE_TIMEOUT_MS,
            CONNECT_TIMEOUT_MS,
            SEND_TIMEOUT_MS,
            RECEIVE_TIMEOUT_MS,
        )
    }
    .map_err(|error| format!("Cannot configure the DeepSeek request timeouts: {error}"))?;

    let host = wide(ENDPOINT_HOST);
    let connection = unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0) };
    let connection = Handle::new(connection, "Cannot connect to the DeepSeek API")?;

    let verb = wide("POST");
    let path = wide(ENDPOINT_PATH);
    let request = unsafe {
        WinHttpOpenRequest(
            connection.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    };
    let request = Handle::new(request, "Cannot create the DeepSeek request")?;

    // The body is UTF-8 JSON built by `request_body`; only the transcript in it comes
    // from the user. The key only appears in this header, never in the body.
    let headers: Vec<u16> =
        format!("Content-Type: application/json\r\nAuthorization: Bearer {key}")
            .encode_utf16()
            .collect();
    let bytes = body.as_bytes();
    unsafe {
        WinHttpSendRequest(
            request.0,
            Some(&headers),
            Some(bytes.as_ptr().cast()),
            bytes.len() as u32,
            bytes.len() as u32,
            0,
        )
    }
    .map_err(|error| format!("Cannot send the refine request to {ENDPOINT_HOST}: {error}"))?;
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
        .map_err(|error| format!("The DeepSeek API did not answer: {error}"))?;
    let status = status_code(request.0)?;
    let response = read_body(request.0)?;
    let response = String::from_utf8(response)
        .map_err(|_| "The DeepSeek API response was not valid UTF-8".to_string())?;
    Ok((status, response))
}

fn status_code(request: *mut core::ffi::c_void) -> AppResult<u16> {
    let mut status: u32 = 0;
    let mut length = std::mem::size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut length,
            std::ptr::null_mut(),
        )
    }
    .map_err(|error| format!("Cannot read the DeepSeek HTTP status: {error}"))?;
    Ok(status as u16)
}

fn read_body(request: *mut core::ffi::c_void) -> AppResult<Vec<u8>> {
    let started = Instant::now();
    let mut response = Vec::new();
    loop {
        if started.elapsed() > TOTAL_TIMEOUT {
            return Err(format!(
                "The DeepSeek API did not answer within {} seconds",
                TOTAL_TIMEOUT.as_secs()
            ));
        }
        let mut available: u32 = 0;
        unsafe { WinHttpQueryDataAvailable(request, &mut available) }
            .map_err(|error| format!("Cannot read the DeepSeek response: {error}"))?;
        if available == 0 {
            break;
        }
        let chunk = available.min(64 * 1024);
        let mut buffer = vec![0u8; chunk as usize];
        let mut read: u32 = 0;
        unsafe { WinHttpReadData(request, buffer.as_mut_ptr().cast(), chunk, &mut read) }
            .map_err(|error| format!("Cannot read the DeepSeek response: {error}"))?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read as usize]);
        if response.len() > MAX_RESPONSE_BYTES {
            return Err(
                "The DeepSeek API response was too large to be one correction, so it was not used"
                    .into(),
            );
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT: &str =
        "我们使用 deep sick harnessness 来做测试。\nSecond line, keep CUDA and 42.";

    #[test]
    fn the_request_body_carries_exactly_the_transcript_and_the_product_parameters() {
        let body = request_body(TRANSCRIPT);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["model"], "deepseek-flash");
        assert_eq!(value["thinking"]["type"], "disabled");
        assert_eq!(value["temperature"], 0);
        assert_eq!(value["stream"], false);
        assert_eq!(value["max_tokens"], 4096);
        let messages = value["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2, "one system prompt and one transcript");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], SYSTEM_PROMPT);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(
            messages[1]["content"], TRANSCRIPT,
            "UTF-8 and newlines must survive the JSON encoding"
        );
        // Nothing else may travel: no endpoint, header material or path.
        assert_eq!(value.as_object().unwrap().len(), 6);
        assert!(!body.contains("Bearer"));
        assert!(!body.contains(ENDPOINT_HOST));
        assert!(!body.contains(".wav"));
        assert_eq!(body.matches("deep sick harnessness").count(), 1);
    }

    #[test]
    fn a_normal_completion_returns_the_corrected_text_without_quotes_or_padding() {
        let body = r#"{"id":"x","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"  我们使用 DeepSeek Harness 来做测试。\nSecond line, keep CUDA and 42.  "}}]}"#;
        assert_eq!(
            parse_response(200, body).unwrap(),
            "我们使用 DeepSeek Harness 来做测试。\nSecond line, keep CUDA and 42."
        );
    }

    #[test]
    fn any_finish_reason_other_than_stop_is_a_failure_not_a_partial_replacement() {
        for reason in [
            "length",
            "content_filter",
            "insufficient_system_resource",
            "aborted",
        ] {
            let body = format!(
                r#"{{"choices":[{{"finish_reason":"{reason}","message":{{"content":"截断的结果"}}}}]}}"#
            );
            let error = parse_response(200, &body).expect_err("non-stop must fail");
            assert!(error.contains(reason), "{error}");
            assert!(error.contains("not applied"), "{error}");
        }
    }

    #[test]
    fn a_response_without_a_finish_reason_is_a_failure() {
        let body = r#"{"choices":[{"message":{"content":"文本"}}]}"#;
        assert!(
            parse_response(200, body)
                .unwrap_err()
                .contains("finish reason")
        );
    }

    #[test]
    fn malformed_json_is_a_failure() {
        let error = parse_response(200, "not json at all").unwrap_err();
        assert!(error.contains("not valid JSON"), "{error}");
    }

    #[test]
    fn a_missing_completion_choice_is_a_failure() {
        let error = parse_response(200, r#"{"choices":[]}"#).unwrap_err();
        assert!(error.contains("completion choice"), "{error}");
    }

    #[test]
    fn an_empty_correction_is_a_failure_so_the_local_result_is_kept() {
        let body = r#"{"choices":[{"finish_reason":"stop","message":{"content":"   "}}]}"#;
        let error = parse_response(200, body).unwrap_err();
        assert!(error.contains("empty correction"), "{error}");
    }

    #[test]
    fn an_http_error_reports_the_status_and_the_vendor_message() {
        let body = r#"{"error":{"message":"Authentication Fails (no such user)","type":"authentication_error"}}"#;
        let error = parse_response(401, body).unwrap_err();
        assert!(error.contains("HTTP 401"), "{error}");
        assert!(
            error.contains("Authentication Fails (no such user)"),
            "{error}"
        );
    }

    #[test]
    fn an_http_error_with_a_non_json_body_is_clipped_instead_of_echoing_a_huge_page() {
        let body = "x".repeat(10_000);
        let error = parse_response(502, &body).unwrap_err();
        assert!(error.contains("HTTP 502"), "{error}");
        assert!(
            error.chars().count() < 260,
            "the error body must be clipped: {} chars",
            error.chars().count()
        );
    }

    #[test]
    fn a_missing_key_is_an_availability_error_and_never_sends_anything() {
        let client = DeepSeek { key: None };
        let error = client.availability().unwrap_err();
        assert!(error.contains("DEEPSEEK_API_KEY"), "{error}");
        // `refine` refuses before any network work.
        assert!(
            client
                .refine("文本")
                .unwrap_err()
                .contains("DEEPSEEK_API_KEY")
        );
    }

    #[test]
    fn an_empty_environment_value_counts_as_no_key() {
        assert_eq!(normalize_key(Some("   ".into())), None);
        assert_eq!(normalize_key(Some("\t\r\n".into())), None);
        assert_eq!(normalize_key(None), None);
        assert_eq!(
            normalize_key(Some("  sk-local-key  ".into())).as_deref(),
            Some("sk-local-key")
        );
    }
}
