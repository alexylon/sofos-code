use super::types::{ContentBlock, CreateMessageResponse, Usage};
use crate::error::{Result, SofosError};
use colored::Colorize;
use rand::RngExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use std::future::Future;
use std::time::Duration;

/// Client-level ceiling for the main LLM providers (Anthropic, OpenAI).
/// reqwest's `.timeout()` is a total-operation deadline (not an idle one),
/// so this has to cover the whole response — including minutes of silent
/// adaptive thinking on Opus 4.7+ at high effort before any tokens arrive.
/// 30 min fits every practical request we've seen; anything longer is
/// almost certainly a stuck connection rather than a legitimately-thinking
/// model.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(1800);
/// Morph's edit endpoint returns in sub-second under normal load, but
/// can stall on large files or under backend pressure. 10 min is the
/// ceiling the tool dispatcher enforces before falling back to
/// `edit_file`; we mirror it here so the client-level timeout never
/// kills a request the dispatcher would still be happy to wait for.
pub const MORPH_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
pub const MAX_RETRIES: u32 = 2;
pub const INITIAL_RETRY_DELAY_MS: u64 = 1000;
const JITTER_FACTOR: f64 = 0.3; // Add 0-30% random jitter

/// Default `Content-Type` applied by [`build_http_client`] when the
/// caller didn't set one. Every provider we integrate with speaks JSON.
const DEFAULT_CONTENT_TYPE: &str = "application/json";

/// Response shape constants returned to the rest of the crate. All our
/// providers assemble the same `{ response_type: "message", role:
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
/// authentication / version headers, and the caller-chosen client-level
/// timeout. Returns a friendly `Config` error instead of a raw reqwest
/// error so the surface mirrors the rest of the crate.
pub fn build_http_client(
    provider_headers: HeaderMap,
    timeout: Duration,
) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .default_headers(merge_default_headers(provider_headers))
        .timeout(timeout)
        .build()
        .map_err(|e| SofosError::Config(format!("Failed to create HTTP client: {}", e)))
}

/// Assemble a [`CreateMessageResponse`] from the parts every provider
/// extracts from its own response shape. Centralises the three
/// non-provider-specific constant fields (`response_type`, `role`,
/// wrapping `Usage`) so adding one more doesn't require touching every
/// client.
pub fn build_message_response(
    id: String,
    model: String,
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    usage: Usage,
) -> CreateMessageResponse {
    CreateMessageResponse {
        id,
        response_type: RESPONSE_TYPE_MESSAGE.to_string(),
        role: ROLE_ASSISTANT.to_string(),
        content,
        model,
        stop_reason,
        usage,
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
    // trim the trailing comma if we cut mid-key, and tack on `}`. Build
    // on top of `escaped` rather than re-running the escape pass on
    // `trimmed`, so the intra-JSON trailing-comma stripping done above
    // isn't discarded for this attempt.
    if escaped.starts_with('{') {
        let mut candidate = escaped.clone();
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
    // Redact `sk-…` / `Bearer …` shapes before tracing — the preview
    // can land in transcripts and crash reports.
    let preview = redact_api_secrets(&args[..preview_end]);
    tracing::warn!(
        tool = %name,
        preview = %preview,
        "failed to parse tool arguments as JSON; passing raw_arguments through"
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

/// Upper bound on the provider-error body interpolated into a user-facing
/// `SofosError::Api`. A misconfigured proxy that returns a multi-MB HTML
/// page, or a moderation block that echoes the whole request, otherwise
/// floods stderr and the status line. Beyond this the body is truncated
/// with a `[…N more bytes elided]` marker so the model still gets some
/// signal but the UI stays readable.
pub const MAX_PROVIDER_ERROR_BODY_BYTES: usize = 4 * 1024;

/// Upper bound on the SSE re-assembly buffer used by both the Anthropic
/// and OpenAI streaming parsers. A server (or a middlebox) that streams
/// gigabytes without a newline would otherwise grow `buffer` until the
/// 30-minute request timeout fires, exhausting memory long before the
/// timeout helps. 16 MB is far above any legitimate single SSE line we
/// have seen in practice.
pub const MAX_SSE_BUFFER_BYTES: usize = 16 * 1024 * 1024;

/// Best-effort redaction of API-key-shaped substrings inside a provider
/// error body. Provider 401 responses sometimes echo the rejected key
/// (truncated or otherwise), which would land verbatim in transcripts
/// and crash reports. Scans for `sk-…` style prefixes and `Bearer …`
/// pairs and rewrites each run as `<keyword>[redacted]`. Caller is
/// expected to apply [`truncate_at_char_boundary`] separately if the
/// body needs a length cap.
pub fn redact_api_secrets(body: &str) -> String {
    /// Minimum byte count for a `sk-…` run we treat as a real key.
    /// Below this, the prefix is just an unrelated `sk-` substring
    /// (a CSS class, an error code, a stray identifier).
    const SK_KEY_MIN_LEN: usize = 11;
    /// Same idea on the bearer side, sized against the random tail
    /// that follows the `Bearer ` prefix.
    const BEARER_TAIL_MIN_LEN: usize = 8;
    const BEARER_PREFIX_LEN: usize = "Bearer ".len();

    fn is_key_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
    }

    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"sk-") {
            let mut end = i + 3;
            while end < bytes.len() && is_key_byte(bytes[end]) {
                end += 1;
            }
            if end - i >= SK_KEY_MIN_LEN {
                out.push_str("sk-[redacted]");
                i = end;
                continue;
            }
        }
        if bytes[i..].starts_with(b"Bearer ") || bytes[i..].starts_with(b"bearer ") {
            let mut end = i + BEARER_PREFIX_LEN;
            while end < bytes.len() && is_key_byte(bytes[end]) {
                end += 1;
            }
            if end - i >= BEARER_PREFIX_LEN + BEARER_TAIL_MIN_LEN {
                out.push_str(&body[i..i + BEARER_PREFIX_LEN]);
                out.push_str("[redacted]");
                i = end;
                continue;
            }
        }
        // Non-ASCII bytes carry one full UTF-8 char; push it whole so
        // the surrounding text stays valid.
        let ch = body[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Truncate `body` to [`MAX_PROVIDER_ERROR_BODY_BYTES`] and run
/// [`redact_api_secrets`] over the result. Centralised so every error-
/// to-message hop applies the same cleanup.
pub fn sanitize_provider_error_body(body: &str) -> String {
    let cut = truncate_at_char_boundary(body, MAX_PROVIDER_ERROR_BODY_BYTES);
    let mut truncated = redact_api_secrets(&body[..cut]);
    if body.len() > cut {
        let extra = body.len() - cut;
        truncated.push_str(&format!(" […{} more bytes elided]", extra));
    }
    truncated
}

/// Upper bound applied to the `Retry-After` value advertised by a 429
/// response. 60 seconds is comfortably above the burst-limit windows
/// the APIs we integrate with use in practice, and short enough that
/// an extreme value (a misbehaving or malicious server asking for
/// hours) can't lock sofos for an unreasonable wait.
const MAX_RATE_LIMIT_RETRY_AFTER: Duration = Duration::from_secs(60);

/// `ServerError` and `RateLimited` trigger a retry — transport failures
/// and other 4xx statuses fail fast. `RateLimited` carries the
/// `Retry-After` value the server asked for, capped at
/// [`MAX_RATE_LIMIT_RETRY_AFTER`]; the retry loop is also capped at one
/// extra attempt for this variant so an ongoing limit doesn't burn
/// through every retry slot waiting.
#[derive(Debug)]
pub enum ApiCallError {
    Transport(reqwest::Error),
    /// Body already drained for error reporting.
    ServerError {
        status: reqwest::StatusCode,
        body: String,
    },
    /// HTTP 429. Body already drained for error reporting.
    RateLimited {
        retry_after: Option<Duration>,
        body: String,
    },
    /// Body already drained for error reporting.
    ClientError {
        status: reqwest::StatusCode,
        body: String,
    },
}

impl ApiCallError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::ServerError { .. } | Self::RateLimited { .. })
    }

    fn describe(&self) -> String {
        match self {
            Self::Transport(e) => format!("Request failed: {}", e),
            Self::ServerError { status, .. } => format!("Server error {}", status),
            Self::RateLimited { retry_after, .. } => match retry_after {
                Some(d) => format!("Rate limited (retry after {:?})", d),
                None => "Rate limited".to_string(),
            },
            Self::ClientError { status, .. } => format!("Client error {}", status),
        }
    }
}

/// Read the `Retry-After` header in its seconds-since-now form and clamp
/// the result to [`MAX_RATE_LIMIT_RETRY_AFTER`]. RFC 7231 also allows an
/// HTTP-date form, but every API we integrate with uses the seconds
/// form for 429s, so the date form falls back to `None` and the retry
/// loop uses its default exponential delay.
fn parse_retry_after_header(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .map(|d| d.min(MAX_RATE_LIMIT_RETRY_AFTER))
}

/// Drains the body on non-2xx so the caller can report it; 2xx responses
/// are returned untouched (important for streaming callers that consume
/// the body later).
pub async fn classify_response(
    response: reqwest::Response,
) -> std::result::Result<reqwest::Response, ApiCallError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    // Grab `Retry-After` before draining the body — `text().await`
    // consumes the response and the headers go with it.
    let retry_after = if status.as_u16() == 429 {
        parse_retry_after_header(response.headers())
    } else {
        None
    };
    let body = response.text().await.unwrap_or_default();
    if status.is_server_error() {
        Err(ApiCallError::ServerError { status, body })
    } else if status.as_u16() == 429 {
        Err(ApiCallError::RateLimited { retry_after, body })
    } else {
        Err(ApiCallError::ClientError { status, body })
    }
}

/// Used as the operation closure inside [`with_retries`].
pub async fn send_classified(
    request: reqwest::RequestBuilder,
) -> std::result::Result<reqwest::Response, ApiCallError> {
    let response = request.send().await.map_err(ApiCallError::Transport)?;
    classify_response(response).await
}

/// Use this when a retry would re-burn an expensive call — the main
/// Anthropic and OpenAI endpoints, where a 5xx or timeout is surfaced to
/// the user immediately rather than quietly re-running a long thinking
/// phase.
pub async fn send_once(
    service_name: &str,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response> {
    send_classified(request)
        .await
        .map_err(|e| api_call_error_to_sofos(service_name, 1, e))
}

fn api_call_error_to_sofos(service_name: &str, attempts: u32, e: ApiCallError) -> SofosError {
    match e {
        ApiCallError::Transport(err) => SofosError::NetworkError(format!(
            "{} request failed after {} attempt(s): {}",
            service_name, attempts, err
        )),
        ApiCallError::ServerError { status, body } | ApiCallError::ClientError { status, body } => {
            SofosError::Api(format!(
                "{} request failed with status {} after {} attempt(s): {}",
                service_name,
                status,
                attempts,
                sanitize_provider_error_body(&body)
            ))
        }
        ApiCallError::RateLimited { retry_after, body } => SofosError::Api(format!(
            "{} rate-limited (HTTP 429{}) after {} attempt(s): {}",
            service_name,
            match retry_after {
                Some(d) => format!(", server asked for {:?}", d),
                None => String::new(),
            },
            attempts,
            sanitize_provider_error_body(&body)
        )),
    }
}

/// Retries 5xx responses and 429 rate-limit responses; transport
/// failures and other 4xx statuses fail fast, since retrying those
/// either re-burns expensive work or re-hits a deterministic client
/// error. A 429 is retried at most once, using the server-supplied
/// `Retry-After` delay (capped at [`MAX_RATE_LIMIT_RETRY_AFTER`]) when
/// present and the exponential-backoff delay otherwise.
pub async fn with_retries<F, Fut, T>(service_name: &str, operation: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = std::result::Result<T, ApiCallError>>,
{
    let mut retry_delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS);
    let mut next_delay_override: Option<Duration> = None;
    let mut rate_limit_attempts: u32 = 0;
    const MAX_RATE_LIMIT_RETRIES: u32 = 1;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            // Server-supplied `Retry-After` wins over the
            // exponential-backoff schedule for one iteration. Jitter is
            // applied either way so a synchronised retry storm from
            // many clients on the same shared limit doesn't all wake
            // up at the same instant.
            let base_delay = next_delay_override.take().unwrap_or(retry_delay);
            let jitter = rand::rng().random_range(0.0..JITTER_FACTOR);
            let jittered_delay = base_delay.mul_f64(1.0 + jitter);

            tracing::warn!(
                service = service_name,
                attempt = attempt,
                max_retries = MAX_RETRIES,
                delay_ms = jittered_delay.as_millis() as u64,
                "Retrying API request after retryable error"
            );
            eprintln!(
                " {} retrying in {:?}... (attempt {}/{})",
                format!("{}:", service_name).bright_yellow(),
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
                let retryable = e.is_retryable();
                let is_rate_limited = matches!(e, ApiCallError::RateLimited { .. });
                if is_rate_limited {
                    rate_limit_attempts += 1;
                    if let ApiCallError::RateLimited {
                        retry_after: Some(d),
                        ..
                    } = &e
                    {
                        next_delay_override = Some(*d);
                    }
                }
                let rate_limit_cap_reached =
                    is_rate_limited && rate_limit_attempts > MAX_RATE_LIMIT_RETRIES;
                if attempt < MAX_RETRIES && retryable && !rate_limit_cap_reached {
                    continue;
                }
                let attempts = attempt + 1;
                tracing::error!(
                    service = service_name,
                    attempts = attempts,
                    reason = %e.describe(),
                    retryable = retryable,
                    "API request failed permanently"
                );
                return Err(api_call_error_to_sofos(service_name, attempts, e));
            }
        }
    }

    // for-loop always returns inside; this is just to satisfy the type checker.
    Err(SofosError::NetworkError(format!(
        "Unknown {} error",
        service_name
    )))
}

/// Test-only helpers for driving the streaming parsers without an HTTP
/// layer. Used by both `anthropic::tests` and `openai::tests` so the SSE
/// wire format lives in one place.
#[cfg(test)]
pub(crate) mod sse_test_support {
    use crate::error::Result;
    use futures::stream::{self, Stream};
    use serde_json::Value;

    /// Build a synthetic SSE byte stream from a list of JSON events.
    /// Each event is emitted as a single `data: {...}\n` chunk — adequate
    /// for parser tests that don't need to exercise chunk-boundary
    /// reassembly. Callers wanting to split events across chunks should
    /// build their own iterator directly.
    pub(crate) fn sse_stream_from_events(
        events: Vec<Value>,
    ) -> impl Stream<Item = Result<Vec<u8>>> + Unpin {
        let chunks: Vec<Result<Vec<u8>>> = events
            .into_iter()
            .map(|event| {
                let line = format!(
                    "data: {}\n",
                    serde_json::to_string(&event).expect("event is JSON-serializable")
                );
                Ok(line.into_bytes())
            })
            .collect();
        stream::iter(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_strips_sk_keys_and_bearer_tokens() {
        let body = "Invalid x-api-key: sk-ant-api03-AAAAaaaa1111BBBBbbbb22 returned error";
        let cleaned = redact_api_secrets(body);
        assert!(
            !cleaned.contains("sk-ant-api03"),
            "key prefix must be removed, got: {cleaned}"
        );
        assert!(cleaned.contains("sk-[redacted]"));
        assert!(cleaned.contains("returned error"));

        let bearer = "Authorization: Bearer abcdefghijKLMN1234 expired";
        let cleaned = redact_api_secrets(bearer);
        assert!(
            cleaned.contains("Bearer [redacted]"),
            "bearer token must be redacted, got: {cleaned}"
        );
        assert!(cleaned.contains("expired"));

        // Short fragments that LOOK like a key but lack enough chars
        // after `sk-` stay untouched (avoids redacting unrelated
        // `sk-` substrings).
        let small = "see sk-x for details";
        let unchanged = redact_api_secrets(small);
        assert_eq!(unchanged, small);
    }

    #[test]
    fn sanitize_body_caps_long_payload_with_marker() {
        let payload = "Z".repeat(MAX_PROVIDER_ERROR_BODY_BYTES * 3);
        let out = sanitize_provider_error_body(&payload);
        assert!(out.len() < payload.len());
        assert!(out.contains("more bytes elided"));
    }

    #[test]
    fn sanitize_body_redacts_and_truncates_together() {
        // Key followed by a long tail must lose the key AND keep the
        // elision marker for the trailing bytes.
        let payload = format!(
            "key=sk-ant-api03-AAAAaaaa1111BBBB tail{}",
            "Y".repeat(MAX_PROVIDER_ERROR_BODY_BYTES * 2)
        );
        let out = sanitize_provider_error_body(&payload);
        assert!(out.contains("sk-[redacted]"));
        assert!(out.contains("more bytes elided"));
    }

    #[test]
    fn api_call_error_is_retryable_for_server_error_and_rate_limited() {
        let server = ApiCallError::ServerError {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            body: String::new(),
        };
        let rate_limited = ApiCallError::RateLimited {
            retry_after: Some(Duration::from_secs(2)),
            body: String::new(),
        };
        let client = ApiCallError::ClientError {
            status: reqwest::StatusCode::BAD_REQUEST,
            body: String::new(),
        };
        assert!(server.is_retryable());
        assert!(rate_limited.is_retryable());
        assert!(!client.is_retryable());
    }

    #[tokio::test]
    async fn with_retries_retries_rate_limited_once_then_surrenders() {
        // 429 used to fall into `ClientError` and fail on the first
        // attempt; now it retries exactly once, honouring the
        // server-supplied delay but capped so a long limit doesn't
        // burn through every retry slot.
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result: Result<&'static str> = with_retries("Test", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                Err(ApiCallError::RateLimited {
                    retry_after: Some(Duration::from_millis(1)),
                    body: "slow down".into(),
                })
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "rate-limited responses retry exactly once (one initial attempt plus one retry)"
        );
    }

    #[tokio::test]
    async fn with_retries_rate_limited_then_success_returns_value() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result: Result<&'static str> = with_retries("Test", || {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(ApiCallError::RateLimited {
                        retry_after: Some(Duration::from_millis(1)),
                        body: "slow down".into(),
                    })
                } else {
                    Ok("done")
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), "done");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parse_retry_after_reads_seconds_form() {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("7"));
        assert_eq!(
            parse_retry_after_header(&headers),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn parse_retry_after_clamps_oversized_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("9999999"),
        );
        assert_eq!(
            parse_retry_after_header(&headers),
            Some(MAX_RATE_LIMIT_RETRY_AFTER)
        );
    }

    #[test]
    fn parse_retry_after_returns_none_for_http_date_form() {
        // The HTTP-date form is valid per RFC 7231 but no API we
        // integrate with uses it for 429. Falling back to `None`
        // lets the retry loop pick its default exponential delay
        // rather than hard-failing on the parse.
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert!(parse_retry_after_header(&headers).is_none());
    }

    #[test]
    fn parse_retry_after_returns_none_when_header_absent() {
        assert!(parse_retry_after_header(&HeaderMap::new()).is_none());
    }

    #[tokio::test]
    async fn with_retries_retries_server_error_then_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result: Result<&'static str> = with_retries("Test", || {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(ApiCallError::ServerError {
                        status: reqwest::StatusCode::BAD_GATEWAY,
                        body: "retry me".into(),
                    })
                } else {
                    Ok("done")
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), "done");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn with_retries_does_not_retry_client_error() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result: Result<&'static str> = with_retries("Test", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                Err(ApiCallError::ClientError {
                    status: reqwest::StatusCode::BAD_REQUEST,
                    body: "nope".into(),
                })
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

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
        assert!(build_http_client(HeaderMap::new(), REQUEST_TIMEOUT).is_ok());
    }

    #[test]
    fn build_message_response_populates_constant_fields() {
        let r = build_message_response(
            "id-42".into(),
            "test-model".into(),
            vec![],
            Some("max_tokens".into()),
            Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: Some(40),
                cache_creation_input_tokens: Some(10),
            },
        );
        assert_eq!(r.id, "id-42");
        assert_eq!(r.model, "test-model");
        assert_eq!(r.role, "assistant");
        assert_eq!(r.response_type, "message");
        assert_eq!(r.stop_reason.as_deref(), Some("max_tokens"));
        assert_eq!(r.usage.input_tokens, 100);
        assert_eq!(r.usage.output_tokens, 50);
        assert_eq!(r.usage.cache_read_input_tokens, Some(40));
        assert_eq!(r.usage.cache_creation_input_tokens, Some(10));
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
    fn parse_args_truncated_payload_keeps_intra_json_trailing_comma_fix() {
        // Regression: the truncation-repair branch used to re-build
        // its candidate from `trimmed` and re-run only the control-char
        // escape. The intra-JSON `,]` / `,}` strip done earlier was
        // discarded for this branch, so a payload that needed BOTH
        // repairs (an internal trailing comma AND the missing closing
        // brace) fell through to the raw-arguments fallback.
        let v = parse_tool_arguments("write_file", r#"{"path":"a","items":[1,2,]"#);
        assert_eq!(
            v["path"], "a",
            "the truncation branch must consume the comma-stripped intermediate"
        );
        let items = v["items"].as_array().expect("items array recovered");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], 1);
        assert_eq!(items[1], 2);
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
