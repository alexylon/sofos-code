//! HTTP client, connectivity check, and the non-streaming
//! [`AnthropicClient::create_message`] path. The streaming path lives
//! in [`super::stream`]; the request-shape and beta-header helpers
//! live in [`super::wire`].

use crate::api::anthropic::wire::{BETA_HEADER_NAME, anthropic_beta_for, prepare_request};
use crate::api::types::{CreateMessageRequest, CreateMessageResponse};
use crate::api::utils;
use crate::error::{Result, SofosError};
use reqwest::header::{HeaderMap, HeaderValue};

pub(super) const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1";

#[derive(Clone)]
pub struct AnthropicClient {
    pub(super) client: reqwest::Client,
}

/// Anthropic API version pin sent on every request. Bump only after
/// re-validating the wire format against the live API — Anthropic
/// has shipped breaking schema changes on minor version bumps
/// before (the `usage.input_tokens` semantics for cache-read
/// flipped silently between 2024-06 and 2024-10 versions).
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

impl AnthropicClient {
    pub fn new(api_key: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&api_key)
                .map_err(|e| SofosError::Config(format!("Invalid API key format: {}", e)))?,
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_API_VERSION),
        );
        // `anthropic-beta` is set per-request by `anthropic_beta_for`
        // so the compaction beta only ships when the target model
        // actually supports it.

        let client = utils::build_http_client(headers, utils::REQUEST_TIMEOUT)?;

        Ok(Self { client })
    }

    /// Check if we can reach the API endpoint
    pub async fn check_connectivity(&self) -> Result<()> {
        utils::check_api_connectivity(
            &self.client,
            ANTHROPIC_API_BASE,
            "Anthropic",
            "https://status.anthropic.com",
        )
        .await
    }

    pub async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        let url = format!("{}/messages", ANTHROPIC_API_BASE);
        let request = prepare_request(request);
        let beta = anthropic_beta_for(&request.model);

        let response = utils::send_once(
            "Anthropic",
            self.client
                .post(&url)
                .header(BETA_HEADER_NAME, beta)
                .json(&request),
        )
        .await?;

        let result = response.json::<CreateMessageResponse>().await?;
        Ok(result)
    }
}
