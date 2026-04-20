use super::types::{ContentBlock, CreateMessageResponse, Usage};
use crate::error::{Result, SofosError};
use colored::Colorize;
use rand::RngExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use std::future::Future;
use std::time::Duration;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Per-request ceiling for streaming (SSE) endpoints. Larger than
/// [`REQUEST_TIMEOUT`] because a single stream can legitimately run
/// for several minutes while the model is producing a long reply plus
/// extended-thinking tokens; the 300s non-streaming budget would clip
/// those off mid-stream.
pub const STREAMING_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
pub const MAX_RETRIES: u32 = 2;
pub const INITIAL_RETRY_DELAY_MS: u64 = 1000;
const JITTER_FACTOR: f64 = 0.3; // Add 0-30% random jitter

/// Default `Content-Type` applied by [`build_http_client`] when the
/// caller didn't set one. Every provider we integrate with speaks JSON.
const DEFAULT_CONTENT_TYPE: &str = "application/json";

/// Response shape constants returned to the rest of the crate. All our
/// providers assemble the same `{ _response_type: "message", _role:
/// "assistant" }` shell; centralised here so a protocol change lands
/// in one place.
const RESPONSE_TYPE_MESSAGE: &str = "message";
const ROLE_ASSISTANT: &str = "assistant";

/// Merge the caller's provider-specific headers (auth, API version,
/// custom beta flags) with the default `Content-Type: application/json`
/// the rest of the crate expects. Exposed separately from
/// [`build_http_client`] so unit tests can assert the merge behaviour
/// without a live reqwest client (whose `default_headers` are opaque
/// once the `Client` is built).
fn merge_default_headers(provider_headers: HeaderMap) -> HeaderMap {
    let mut headers = provider_headers;
    headers
        .entry(CONTENT_TYPE)
        .or_insert(HeaderValue::from_static(DEFAULT_CONTENT_TYPE));
    headers
}

/// Build the standard JSON-over-HTTPS client every provider uses:
/// common `Content-Type: application/json`, the caller's
/// authentication / version headers, and the shared [`REQUEST_TIMEOUT`].
/// Returns a friendly `Config` error instead of a raw reqwest error so
/// the surface mirrors the rest of the crate.
pub fn build_http_client(provider_headers: HeaderMap) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .default_headers(merge_default_headers(provider_headers))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| SofosError::Config(format!("Failed to create HTTP client: {}", e)))
}

/// Assemble a [`CreateMessageResponse`] from the parts every provider
/// extracts from its own response shape. Centralises the three
/// non-provider-specific constant fields (`_response_type`, `_role`,
/// wrapping `Usage`) so adding one more doesn't require touching every
/// client.
pub fn build_message_response(
    id: String,
    model: String,
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    input_tokens: u32,
    output_tokens: u32,
) -> CreateMessageResponse {
    CreateMessageResponse {
        _id: id,
        _response_type: RESPONSE_TYPE_MESSAGE.to_string(),
        _role: ROLE_ASSISTANT.to_string(),
        content,
        _model: model,
        stop_reason,
        usage: Usage {
            input_tokens,
            output_tokens,
        },
    }
}

/// Check connectivity to an API endpoint with a 5-second timeout.
pub async fn check_api_connectivity(
    client: &reqwest::Client,
    base_url: &str,
    provider_name: &str,
    status_url: &str,
) -> Result<()> {
    match tokio::time::timeout(Duration::from_secs(5), client.head(base_url).send()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(SofosError::NetworkError(format!(
            "Cannot reach {} API. Please check:\n  \
             1. Your internet connection\n  \
             2. Firewall/proxy settings\n  \
             3. API status at {}\n\
             Original error: {}",
            provider_name, status_url, e
        ))),
        Err(_) => Err(SofosError::NetworkError(
            "Connection timeout. Please check your network connection.".into(),
        )),
    }
}

/// Maximum number of bytes from the raw tool-arguments string we echo
/// to stderr when every repair strategy fails. 500 is enough to see
/// both the opening fields and the point where the JSON went wrong
/// without dumping a full multi-KB `content` payload into the log.
const UNPARSEABLE_ARGS_PREVIEW_BYTES: usize = 500;

/// Parse tool-call arguments emitted by an LLM provider into a JSON value.
///
/// Both Anthropic and OpenAI deliver function/tool arguments as a JSON
/// string — via streaming `input_json_delta` events on Anthropic, and as
/// a fully-serialized `arguments` field on OpenAI. Either channel can
/// yield structurally broken JSON when the response gets cut off by
/// `max_tokens`, the model emits a raw newline inside a string value,
/// or (on some OpenAI variants) the payload is double-encoded.
///
/// The repair ladder — trim, drop trailing commas, escape raw control
/// chars inside strings, close an unterminated string, add a missing
/// closing brace, unwrap one level of double-encoding — recovers the
/// vast majority of malformed payloads. When all steps fail we fall
/// back to `{"raw_arguments": args}` so the per-tool dispatcher can
/// surface a "missing parameter" error including the keys that were
/// recovered, which the model then self-corrects from on the next turn.
///
/// `morph_edit_file` opts out of repair: its `code_edit` field carries
/// arbitrary source code, and a "successful" repair of a truncated
/// payload would silently merge corrupted code into a user file.
pub fn parse_tool_arguments(name: &str, args: &str) -> serde_json::Value {
    if name == crate::tools::tool_name::ToolName::MorphEditFile.as_str() {
        return serde_json::from_str(args)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
    }

    if let Some(v) = try_parse_json_object(args) {
        return v;
    }

    let trimmed = args.trim();
    if trimmed != args {
        if let Some(v) = try_parse_json_object(trimmed) {
            return v;
        }
    }

    let no_trailing_commas = strip_trailing_commas_outside_strings(trimmed);
    if no_trailing_commas != trimmed {
        if let Some(v) = try_parse_json_object(&no_trailing_commas) {
            return v;
        }
    }

    // Escape raw newlines / carriage returns / tabs INSIDE string
    // literals. Models routinely emit multi-line `content` values with
    // literal `\n` bytes (prose of a long document, source code with
    // actual newlines), which JSON rejects. This single repair recovers
    // the vast majority of malformed payloads.
    let escaped = escape_control_chars_in_json_strings(&no_trailing_commas);
    if escaped != no_trailing_commas {
        if let Some(v) = try_parse_json_object(&escaped) {
            return v;
        }
    }

    // Truncated mid-string: the response terminated without closing the
    // open string literal and the enclosing object. Close the string,
    // trim the trailing comma if we cut mid-key, and tack on `}`.
    if trimmed.starts_with('{') {
        let mut candidate = escape_control_chars_in_json_strings(trimmed);
        if string_is_open(&candidate) {
            candidate.push('"');
        }
        candidate = candidate.trim_end_matches(',').to_string();
        if !candidate.ends_with('}') {
            candidate.push('}');
        }
        if let Some(v) = try_parse_json_object(&candidate) {
            return v;
        }
    }

    let preview_end = truncate_at_char_boundary(args, UNPARSEABLE_ARGS_PREVIEW_BYTES);
    eprintln!(
        "  \x1b[33m⚠\x1b[0m Failed to parse tool arguments as JSON for {}: {}",
        name,
        &args[..preview_end]
    );
    serde_json::json!({"raw_arguments": args})
}

/// Parse `s` as JSON; if the parse succeeds but yields a bare string
/// that itself looks like a JSON object/array, unwrap one level of
/// encoding. Some OpenAI clients double-encode the `arguments` field.
fn try_parse_json_object(s: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    if let serde_json::Value::String(inner) = &v {
        let inner_trim = inner.trim();
        if inner_trim.starts_with('{') || inner_trim.starts_with('[') {
            if let Ok(unwrapped) = serde_json::from_str::<serde_json::Value>(inner) {
                return Some(unwrapped);
            }
        }
    }
    Some(v)
}

/// Drop `,` characters that immediately precede a closing `}` or `]` —
/// the trailing-comma form that models sometimes emit — but only when
/// the comma sits *outside* a JSON string literal. A pre-existing
/// naive `String::replace(",}", "}")` pass would silently corrupt a
/// user-provided string value whose content happened to contain the
/// two-byte sequence `,}` (e.g. `{"note":"see ,}end"}`). The walker
/// tracks quoting + backslash escapes so the transform only touches
/// structural commas.
fn strip_trailing_commas_outside_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut prev_backslash = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if prev_backslash {
                prev_backslash = false;
            } else if ch == '\\' {
                prev_backslash = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            ',' if matches!(chars.peek(), Some('}') | Some(']')) => {
                // Drop the comma; the next iteration consumes and
                // writes the brace / bracket itself.
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Rewrite raw `\n`, `\r`, `\t` bytes that appear *inside* JSON string
/// literals into their escaped form (`\n`, `\r`, `\t`) while leaving
/// bytes outside strings untouched. Tracks `"` nesting with backslash
/// awareness so already-escaped quotes don't flip the state.
fn escape_control_chars_in_json_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut prev_backslash = false;
    for ch in s.chars() {
        if in_string {
            if prev_backslash {
                // A backslash in a JSON string starts an escape
                // sequence — `\"`, `\\`, `\n`, etc. The next byte is
                // supposed to be a valid escape identifier. If it's
                // instead a raw control char (the emitter forgot to
                // escape it), re-encode it so the sequence becomes a
                // valid escape (`\` + `n` for a raw LF) rather than
                // passing through as `\` + LF, which is invalid JSON
                // and would abort the rest of the repair ladder.
                match ch {
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    _ => out.push(ch),
                }
                prev_backslash = false;
                continue;
            }
            match ch {
                '\\' => {
                    out.push(ch);
                    prev_backslash = true;
                }
                '"' => {
                    out.push(ch);
                    in_string = false;
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(ch),
            }
        } else {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
        }
    }
    out
}

/// Returns `true` if `s` ends with an *unterminated* string literal — a
/// `"` was opened but never matched. Used to decide whether a truncated
/// payload needs a synthetic closing quote before we tack on `}`.
fn string_is_open(s: &str) -> bool {
    let mut in_string = false;
    let mut prev_backslash = false;
    for ch in s.chars() {
        if in_string {
            if prev_backslash {
                prev_backslash = false;
                continue;
            }
            match ch {
                '\\' => prev_backslash = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if ch == '"' {
            in_string = true;
        }
    }
    in_string
}

/// Find the largest byte index <= `max_bytes` that is a valid UTF-8 char boundary.
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub async fn check_response_status(response: reqwest::Response) -> Result<reqwest::Response> {
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        tracing::error!(
            status = %status,
            error = %error_text,
            "API request failed"
        );
        return Err(SofosError::Api(format!(
            "API request failed with status {}: {}",
            status, error_text
        )));
    }
    Ok(response)
}

pub fn is_retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.status().is_some_and(|s| s.is_server_error())
}

/// Execute an async operation with retries and exponential backoff with jitter.
/// Returns the result of the operation or the last error after all retries exhausted.
pub async fn with_retries<F, Fut, T>(service_name: &str, operation: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = std::result::Result<T, reqwest::Error>>,
{
    let mut last_error: Option<reqwest::Error> = None;
    let mut retry_delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS);

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let reason = last_error
                .as_ref()
                .map(|e| {
                    if e.is_timeout() {
                        "Request timed out".to_string()
                    } else {
                        format!("Request failed: {}", e)
                    }
                })
                .unwrap_or_else(|| "Request failed".to_string());

            // Add jitter to prevent thundering herd
            let jitter = rand::rng().random_range(0.0..JITTER_FACTOR);
            let jittered_delay = retry_delay.mul_f64(1.0 + jitter);

            tracing::warn!(
                service = service_name,
                attempt = attempt,
                max_retries = MAX_RETRIES,
                delay_ms = jittered_delay.as_millis() as u64,
                reason = %reason,
                "Retrying API request"
            );

            eprintln!(
                " {} {}, retrying in {:?}... (attempt {}/{})",
                format!("{}:", service_name).bright_yellow(),
                reason,
                jittered_delay,
                attempt,
                MAX_RETRIES
            );
            tokio::time::sleep(jittered_delay).await;
            retry_delay *= 2;
        }

        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                let is_retryable = is_retryable_error(&e);

                if attempt < MAX_RETRIES && is_retryable {
                    last_error = Some(e);
                    continue;
                } else {
                    tracing::error!(
                        service = service_name,
                        attempts = if is_retryable { attempt + 1 } else { 1 },
                        error = %e,
                        retryable = is_retryable,
                        "API request failed permanently"
                    );
                    return Err(SofosError::NetworkError(format!(
                        "{} request failed after {} attempts: {}",
                        service_name,
                        if is_retryable { attempt + 1 } else { 1 },
                        e
                    )));
                }
            }
        }
    }

    Err(last_error.map_or_else(
        || SofosError::NetworkError(format!("Unknown {} error", service_name)),
        |e| SofosError::NetworkError(format!("Failed after {} retries: {}", MAX_RETRIES, e)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_default_headers_adds_content_type_when_absent() {
        let merged = merge_default_headers(HeaderMap::new());
        assert_eq!(merged.get(CONTENT_TYPE).unwrap(), DEFAULT_CONTENT_TYPE);
    }

    #[test]
    fn merge_default_headers_respects_caller_content_type() {
        // If a caller needs e.g. `application/vnd.api+json`, the default
        // must NOT override it.
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.api+json"),
        );
        let merged = merge_default_headers(headers);
        assert_eq!(
            merged.get(CONTENT_TYPE).unwrap(),
            "application/vnd.api+json"
        );
    }

    #[test]
    fn merge_default_headers_preserves_provider_auth_headers() {
        // The merge must leave non-Content-Type headers untouched.
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("secret"));
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        let merged = merge_default_headers(headers);
        assert_eq!(merged.get("x-api-key").unwrap(), "secret");
        assert_eq!(merged.get("anthropic-version").unwrap(), "2023-06-01");
        assert_eq!(merged.get(CONTENT_TYPE).unwrap(), DEFAULT_CONTENT_TYPE);
    }

    #[test]
    fn build_http_client_succeeds_with_empty_headers() {
        // Integration-level smoke: construction doesn't blow up on the
        // minimum viable header set. Header content is covered by the
        // `merge_default_headers_*` tests above.
        assert!(build_http_client(HeaderMap::new()).is_ok());
    }

    #[test]
    fn build_message_response_populates_constant_fields() {
        let r = build_message_response(
            "id-42".into(),
            "test-model".into(),
            vec![],
            Some("max_tokens".into()),
            100,
            50,
        );
        assert_eq!(r._id, "id-42");
        assert_eq!(r._model, "test-model");
        assert_eq!(r._role, "assistant");
        assert_eq!(r._response_type, "message");
        assert_eq!(r.stop_reason.as_deref(), Some("max_tokens"));
        assert_eq!(r.usage.input_tokens, 100);
        assert_eq!(r.usage.output_tokens, 50);
        assert!(r.content.is_empty());
    }

    #[test]
    fn test_truncate_at_char_boundary_ascii() {
        assert_eq!(truncate_at_char_boundary("hello world", 5), 5);
        assert_eq!(truncate_at_char_boundary("hello", 10), 5);
        assert_eq!(truncate_at_char_boundary("hello", 0), 0);
        assert_eq!(truncate_at_char_boundary("", 5), 0);
    }

    #[test]
    fn test_truncate_at_char_boundary_multibyte() {
        // '─' is 3 bytes (U+2500)
        let s = "ab─cd";
        assert_eq!(s.len(), 7); // 2 + 3 + 2
        // Slicing at byte 3 lands inside '─' — should snap to 2
        assert_eq!(truncate_at_char_boundary(s, 3), 2);
        assert_eq!(truncate_at_char_boundary(s, 4), 2);
        // Byte 5 is right after '─'
        assert_eq!(truncate_at_char_boundary(s, 5), 5);
    }

    #[test]
    fn test_truncate_at_char_boundary_emoji() {
        // '🦀' is 4 bytes
        let s = "a🦀b";
        assert_eq!(s.len(), 6); // 1 + 4 + 1
        assert_eq!(truncate_at_char_boundary(s, 1), 1);
        assert_eq!(truncate_at_char_boundary(s, 2), 1);
        assert_eq!(truncate_at_char_boundary(s, 3), 1);
        assert_eq!(truncate_at_char_boundary(s, 4), 1);
        assert_eq!(truncate_at_char_boundary(s, 5), 5);
    }

    use crate::tools::tool_name::ToolName;

    #[test]
    fn parse_args_valid_object_round_trips() {
        let v = parse_tool_arguments("read_file", r#"{"path":"src/main.rs"}"#);
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn parse_args_repairs_trailing_comma() {
        let v = parse_tool_arguments("read_file", r#"{"path":"src/main.rs",}"#);
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn parse_args_repairs_missing_closing_brace() {
        let v = parse_tool_arguments("read_file", r#"{"path":"src/main.rs""#);
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn parse_args_unrepairable_falls_back_to_raw_arguments() {
        let v = parse_tool_arguments("read_file", "not json at all");
        assert_eq!(v["raw_arguments"], "not json at all");
    }

    #[test]
    fn parse_args_escapes_literal_newline_in_string_value() {
        // Models routinely emit multi-line `content` with raw newlines
        // that break the JSON parse. The repair must escape them while
        // leaving structural newlines alone.
        let raw = "{\"path\":\"foo.md\",\"content\":\"line1\nline2\nend\"}";
        let v = parse_tool_arguments("write_file", raw);
        assert_eq!(v["path"], "foo.md");
        assert_eq!(v["content"], "line1\nline2\nend");
    }

    #[test]
    fn parse_args_escapes_newline_in_unicode_content() {
        let raw = "{\"content\":\"# Синергията\nмежду Божия промисъл\",\"path\":\"doc.md\"}";
        let v = parse_tool_arguments("write_file", raw);
        assert_eq!(v["path"], "doc.md");
        assert!(v["content"].as_str().unwrap().contains("Синергията"));
    }

    #[test]
    fn parse_args_recovers_truncated_string_mid_value() {
        let raw = "{\"path\":\"foo.md\",\"content\":\"hello\nworld interrupt";
        let v = parse_tool_arguments("write_file", raw);
        assert_eq!(v["path"], "foo.md");
        assert!(v["content"].as_str().unwrap().contains("hello"));
    }

    #[test]
    fn parse_args_unwraps_double_encoded_object() {
        let raw = r#""{\"path\":\"foo.rs\"}""#;
        let v = parse_tool_arguments("read_file", raw);
        assert_eq!(v["path"], "foo.rs");
    }

    #[test]
    fn parse_args_morph_edit_strict_returns_empty_object_on_failure() {
        // Truncated code_edit must NOT be silently "repaired" — that
        // would merge corrupted source into the user's file. Strict
        // parse → empty object → tool dispatch reports a clear
        // "missing parameter" error.
        let v = parse_tool_arguments(
            ToolName::MorphEditFile.as_str(),
            r#"{"target_filepath":"src/lib.rs","code_edit":"fn x() { let y = [1,2,"#,
        );
        assert!(v.is_object());
        assert_eq!(v.as_object().unwrap().len(), 0);
    }

    #[test]
    fn parse_args_morph_edit_valid_round_trips() {
        let v = parse_tool_arguments(
            ToolName::MorphEditFile.as_str(),
            r#"{"target_filepath":"src/lib.rs","instructions":"add fn","code_edit":"fn x() {}"}"#,
        );
        assert_eq!(v["target_filepath"], "src/lib.rs");
        assert_eq!(v["code_edit"], "fn x() {}");
    }

    #[test]
    fn parse_args_trailing_comma_strip_respects_strings() {
        // The source literally contains the two-byte sequence `,}` inside
        // the `note` string value. A naive `String::replace(",}", "}")`
        // would silently corrupt the user's note; the string-aware walker
        // must leave in-string bytes untouched and only drop the
        // structural trailing comma that precedes the outer `}`.
        let raw = r#"{"note":"list ends ,} here","path":"x.rs",}"#;
        let v = parse_tool_arguments("write_file", raw);
        assert_eq!(v["note"], "list ends ,} here");
        assert_eq!(v["path"], "x.rs");
    }

    #[test]
    fn parse_args_escapes_raw_lf_after_escaped_backslash() {
        // Scenario from the audit: model emits `\\` (2 source chars =
        // escaped backslash = 1 backslash in the decoded value)
        // followed by a raw LF inside a string literal. Before the fix,
        // the `prev_backslash` branch pushed the LF through untouched,
        // leaving invalid JSON that fell through to `raw_arguments`.
        // With the fix, the raw LF gets rewritten to `\n` so the
        // string stays parseable and the decoded value carries exactly
        // `\` + newline (the likely model intent).
        let raw = "{\"path\":\"a.md\",\"content\":\"pre\\\\\npost\"}";
        //                                           ^^^^^^ two-backslash source (escaped) + raw LF
        let v = parse_tool_arguments("write_file", raw);
        assert_eq!(v["path"], "a.md");
        let content = v["content"].as_str().unwrap();
        assert_eq!(
            content, "pre\\\npost",
            "decoded value should be `pre` + backslash + LF + `post`"
        );
    }

    #[test]
    fn parse_args_empty_string_falls_back_to_raw_arguments() {
        let v = parse_tool_arguments("read_file", "");
        assert_eq!(v["raw_arguments"], "");
    }

    #[test]
    fn parse_args_whitespace_only_falls_back_to_raw_arguments() {
        let v = parse_tool_arguments("read_file", "   \n\t");
        assert_eq!(v["raw_arguments"], "   \n\t");
    }

    #[test]
    fn parse_args_array_root_returned_as_is() {
        let v = parse_tool_arguments("read_file", "[1,2,3]");
        assert!(v.is_array());
    }

    #[test]
    fn string_is_open_detects_unterminated_literal() {
        assert!(string_is_open(r#"{"a":"b"#));
        assert!(!string_is_open(r#"{"a":"b"}"#));
        assert!(!string_is_open(r#"{"a":"b\""}"#));
        assert!(string_is_open(r#"{"a":"b\""#)); // \" is escaped, still open
    }

    #[test]
    fn escape_control_chars_leaves_structural_whitespace_alone() {
        let src = "{\n  \"a\": \"b\"\n}";
        assert_eq!(
            escape_control_chars_in_json_strings(src),
            "{\n  \"a\": \"b\"\n}"
        );
    }
}
