//! HTTP client, connectivity check, and the non-streaming
//! [`OpenAIClient::create_message`] path. The streaming path lives in
//! [`super::stream`]; the request body shape, response parsing, and
//! `build_response` conversion live in [`super::wire`].

use crate::api::openai::OpenAIClient;
use crate::api::openai::wire::{OpenAIResponse, build_response, build_responses_body};
use crate::api::types::*;
use crate::api::utils;
use crate::error::{Result, SofosError};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

pub(super) const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

impl OpenAIClient {
    pub fn new(api_key: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|e| SofosError::Config(format!("Invalid API key format: {}", e)))?,
        );

        let client = utils::build_http_client(headers, utils::REQUEST_TIMEOUT)?;

        Ok(Self { client })
    }

    pub async fn check_connectivity(&self) -> Result<()> {
        utils::check_api_connectivity(
            &self.client,
            OPENAI_API_BASE,
            "OpenAI",
            "https://status.openai.com",
        )
        .await
    }

    pub async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        self.call_responses(request).await
    }

    async fn call_responses(&self, request: CreateMessageRequest) -> Result<CreateMessageResponse> {
        let body = build_responses_body(&request);

        if std::env::var("SOFOS_DEBUG").is_ok() {
            if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
                eprintln!("\n=== OpenAI /responses Request ===");
                eprintln!("Sending {} tools to OpenAI", tools.len());
                for tool in tools {
                    if let Some(name) = tool.get("name").and_then(|v| v.as_str()) {
                        eprintln!("  - Tool: {}", name);
                    }
                }
                eprintln!("==================================\n");
            }
        }

        let url = format!("{}/responses", OPENAI_API_BASE);

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI /responses Request Body ===");
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&body)
                    .unwrap_or_else(|_| "Failed to serialize".to_string())
            );
            eprintln!("======================================\n");
        }

        let response = utils::send_once("OpenAI", self.client.post(&url).json(&body)).await?;

        let response_text = response.text().await?;

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI Raw Response ===");
            eprintln!("{}", response_text);
            eprintln!("===========================\n");
        }

        let response_parsed: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| SofosError::Api(format!("Failed to parse OpenAI response: {}", e)))?;

        build_response(response_parsed)
    }
}
