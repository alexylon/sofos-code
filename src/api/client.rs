use super::types::*;
use crate::error::{Result, SofosError};
use futures::stream::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::pin::Pin;
use std::time::Duration;

const API_BASE: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

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

    pub async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        let url = format!("{}/messages", API_BASE);

        let response = self.client.post(&url).json(&request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(SofosError::Api(format!(
                "API request failed with status {}: {}",
                status, error_text
            )));
        }

        let result = response.json::<CreateMessageResponse>().await?;
        Ok(result)
    }

    pub async fn _create_message_stream(
        &self,
        mut request: CreateMessageRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<_StreamEvent>> + Send>>> {
        request.stream = Some(true);
        let url = format!("{}/messages", API_BASE);

        let response = self.client.post(&url).json(&request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(SofosError::Api(format!(
                "API request failed with status {}: {}",
                status, error_text
            )));
        }

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
