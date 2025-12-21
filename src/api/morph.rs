use super::utils::{self, REQUEST_TIMEOUT};
use crate::error::{Result, SofosError};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

const MORPH_BASE_URL: &str = "https://api.morphllm.com/v1";

#[derive(Debug, Clone, Serialize)]
struct MorphMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct MorphRequest {
    model: String,
    messages: Vec<MorphMessage>,
}

#[derive(Debug, Deserialize)]
struct MorphResponse {
    choices: Vec<MorphChoice>,
}

#[derive(Debug, Deserialize)]
struct MorphChoice {
    message: MorphMessageResponse,
}

#[derive(Debug, Deserialize)]
struct MorphMessageResponse {
    content: String,
}

#[derive(Clone)]
pub struct MorphClient {
    client: reqwest::Client,
    model: String,
}

impl MorphClient {
    pub fn new(api_key: String, model: Option<String>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|e| SofosError::Config(format!("Invalid Morph API key: {}", e)))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| SofosError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            model: model.unwrap_or_else(|| "morph-v3-fast".to_string()),
        })
    }

    /// Apply code edits using Morph's fast apply API
    ///
    /// Format requirements:
    /// - instruction: Brief first-person description of changes
    /// - original_code: Complete original code
    /// - code_edit: Updated code with "// ... existing code ..." markers for unchanged sections
    pub async fn apply_edit(
        &self,
        instruction: &str,
        original_code: &str,
        code_edit: &str,
    ) -> Result<String> {
        let content = format!(
            "<instruction>{}</instruction>\n<code>{}</code>\n<update>{}</update>",
            instruction, original_code, code_edit
        );

        let request = MorphRequest {
            model: self.model.clone(),
            messages: vec![MorphMessage {
                role: "user".to_string(),
                content,
            }],
        };

        let url = format!("{}/chat/completions", MORPH_BASE_URL);

        let client = self.client.clone();
        let response = utils::with_retries("Morph", || {
            let client = client.clone();
            let url = url.clone();
            let request = request.clone();
            async move { client.post(&url).json(&request).send().await }
        })
        .await?;

        let response = utils::check_response_status(response).await?;
        let result: MorphResponse = response.json().await?;

        result
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| SofosError::Api("No response from Morph API".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_morph_client_creation() {
        let client = MorphClient::new("test-key".to_string(), None);
        assert!(client.is_ok());
    }
}
