use super::types::*;
use crate::error::{Result, SofosError};
use colored::Colorize;
use futures::stream::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::pin::Pin;
use std::time::Duration;

const API_BASE: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_RETRIES: u32 = 2;
const INITIAL_RETRY_DELAY_MS: u64 = 1000;

#[derive(Clone)]
pub struct AnthropicClient {
    client: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&api_key)
                .map_err(|e| SofosError::Config(format!("Invalid API key format: {}", e)))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| SofosError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { client })
    }

    /// Check if we can reach the API endpoint
    #[allow(dead_code)]
    async fn check_connectivity(&self) -> Result<()> {
        match tokio::time::timeout(Duration::from_secs(5), self.client.head(API_BASE).send()).await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(SofosError::NetworkError(format!(
                "Cannot reach Anthropic API. Please check:\n  \
                 1. Your internet connection\n  \
                 2. Firewall/proxy settings\n  \
                 3. API status at https://status.anthropic.com\n\
                 Original error: {}",
                e
            ))),
            Err(_) => Err(SofosError::NetworkError(
                "Connection timeout. Please check your network connection.".into(),
            )),
        }
    }

    /// Determine if an error is worth retrying
    fn is_retryable_error(error: &reqwest::Error) -> bool {
        // Retry on network errors, timeouts, and some 5xx server errors
        error.is_timeout()
            || error.is_connect()
            || error.status().is_some_and(|s| s.is_server_error())
    }

    pub async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        let url = format!("{}/messages", API_BASE);
        let mut request = request;

        if let Some(tools) = request.tools.take() {
            let filtered: Vec<Tool> = tools
                .into_iter()
                .filter(|t| !matches!(t, Tool::OpenAIWebSearch { tool_type: _ }))
                .collect();

            if !filtered.is_empty() {
                request.tools = Some(filtered);
            }
        }

        // Try with retries
        let mut last_error: Option<reqwest::Error> = None;
        let mut retry_delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS);

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let reason = if let Some(ref err) = last_error {
                    if err.is_timeout() {
                        "Request timed out"
                    } else {
                        &*format!("Request failed: {}", err)
                    }
                } else {
                    "Request failed"
                };

                eprintln!(
                    " {} {}, retrying in {:?}... (attempt {}/{})",
                    "Network:".bright_yellow(),
                    reason,
                    retry_delay,
                    attempt,
                    MAX_RETRIES
                );
                tokio::time::sleep(retry_delay).await;
                retry_delay *= 2; // Exponential backoff
            }

            match self.client.post(&url).json(&request).send().await {
                Ok(response) => {
                    let response = super::utils::check_response_status(response).await?;
                    let result = response.json::<CreateMessageResponse>().await?;
                    return Ok(result);
                }
                Err(e) => {
                    let is_retryable = Self::is_retryable_error(&e);

                    if attempt < MAX_RETRIES && is_retryable {
                        last_error = Some(e);
                        continue;
                    } else {
                        // Final attempt failed or error is not retryable
                        return Err(SofosError::NetworkError(format!(
                            "Failed to complete request after {} attempts.\n\
                             This is usually a temporary network issue. Please try:\n  \
                             1. Check your internet connection\n  \
                             2. Resume this session and continue (your progress is saved)\n  \
                             3. Visit https://status.anthropic.com for API status\n\n\
                             Original error: {}",
                            if is_retryable { attempt + 1 } else { 1 },
                            e
                        )));
                    }
                }
            }
        }

        // This should never be reached, but just in case
        Err(last_error.map_or_else(
            || SofosError::NetworkError("Unknown network error".into()),
            |e| SofosError::NetworkError(format!("Failed after {} retries: {}", MAX_RETRIES, e)),
        ))
    }

    pub async fn _create_message_stream(
        &self,
        mut request: CreateMessageRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<_StreamEvent>> + Send>>> {
        request.stream = Some(true);
        let url = format!("{}/messages", API_BASE);
        let response = self.client.post(&url).json(&request).send().await?;
        let response = super::utils::check_response_status(response).await?;

        let stream = response
            .bytes_stream()
            .map(|result| {
                result.map_err(SofosError::from).and_then(|bytes| {
                    let text = String::from_utf8_lossy(&bytes);
                    _parse_sse_events(&text)
                })
            })
            .flat_map(|result| {
                futures::stream::iter(match result {
                    Ok(events) => events.into_iter().map(Ok).collect::<Vec<_>>(),
                    Err(e) => vec![Err(e)],
                })
            });

        Ok(Box::pin(stream))
    }
}

fn _parse_sse_events(text: &str) -> Result<Vec<_StreamEvent>> {
    let mut events = Vec::new();

    for line in text.lines() {
        if let Some(json_str) = line.strip_prefix("data: ") {
            if json_str.trim() == "[DONE]" {
                break;
            }
            match serde_json::from_str::<_StreamEvent>(json_str) {
                Ok(event) => events.push(event),
                Err(e) => {
                    tracing::warn!("Failed to parse SSE event: {} - {}", e, json_str);
                }
            }
        }
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = AnthropicClient::new("test-key".to_string());
        assert!(client.is_ok());
    }

    #[test]
    fn test_thinking_serialization() {
        let thinking = Thinking::enabled(5120);
        assert_eq!(thinking.thinking_type, "enabled");
        assert_eq!(thinking.budget_tokens, 5120);

        let json = serde_json::to_value(&thinking).unwrap();
        assert_eq!(json["type"], "enabled");
        assert_eq!(json["budget_tokens"], 5120);
    }

    #[test]
    fn test_request_with_thinking() {
        let thinking = Some(Thinking::enabled(3000));
        let request = CreateMessageRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 8192,
            messages: vec![],
            system: None,
            tools: None,
            stream: None,
            thinking,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert!(json["thinking"].is_object());
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 3000);
    }
}
