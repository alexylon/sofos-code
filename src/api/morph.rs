use super::utils::{self, REQUEST_TIMEOUT};
use crate::error::{Result, SofosError};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

const MORPH_BASE_URL: &str = "https://api.morphllm.com/v1";
/// Cap on Morph's output length. Morph's server default is well below
/// this for some model revisions, which silently truncated edits to
/// large files. Set explicitly so we never inherit a smaller limit.
const MORPH_MAX_TOKENS: u32 = 64_000;

#[derive(Debug, Clone, Serialize)]
struct MorphMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct MorphRequest {
    model: String,
    messages: Vec<MorphMessage>,
    max_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct MorphResponse {
    choices: Vec<MorphChoice>,
}

#[derive(Debug, Deserialize)]
struct MorphChoice {
    message: MorphMessageResponse,
    #[serde(default)]
    finish_reason: Option<String>,
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
            max_tokens: MORPH_MAX_TOKENS,
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

        let choice = result
            .choices
            .first()
            .ok_or_else(|| SofosError::Api("No response from Morph API".to_string()))?;

        // Reject responses Morph cut off at the max-tokens boundary —
        // writing them to disk is exactly the scenario that produced
        // silently-truncated files. "stop" / "eos" / absent all mean
        // the model finished normally.
        if let Some(reason) = choice.finish_reason.as_deref() {
            if reason == "length" || reason == "max_tokens" {
                return Err(SofosError::Api(format!(
                    "Morph response was truncated at the token limit (finish_reason={}). \
                     The edit was NOT applied. Use edit_file with explicit old_string / \
                     new_string to make this change instead.",
                    reason
                )));
            }
        }

        Ok(choice.message.content.clone())
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
