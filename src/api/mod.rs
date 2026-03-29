pub mod anthropic;
pub mod morph;
pub mod openai;
pub mod types;
pub mod utils;

pub use anthropic::AnthropicClient;
pub use morph::MorphClient;
pub use openai::OpenAIClient;
pub use types::*;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Clone)]
pub enum LlmClient {
    Anthropic(AnthropicClient),
    OpenAI(OpenAIClient),
}

impl LlmClient {
    pub async fn create_message(
        &self,
        request: types::CreateMessageRequest,
    ) -> crate::error::Result<types::CreateMessageResponse> {
        match self {
            LlmClient::Anthropic(client) => client.create_anthropic_message(request).await,
            LlmClient::OpenAI(client) => client.create_openai_message(request).await,
        }
    }

    pub async fn create_message_streaming<FText, FThink>(
        &self,
        request: types::CreateMessageRequest,
        on_text_delta: FText,
        on_thinking_delta: FThink,
        interrupt_flag: Arc<AtomicBool>,
    ) -> crate::error::Result<types::CreateMessageResponse>
    where
        FText: Fn(&str) + Send + Sync,
        FThink: Fn(&str) + Send + Sync,
    {
        match self {
            LlmClient::Anthropic(client) => {
                client
                    .create_message_streaming(
                        request,
                        on_text_delta,
                        on_thinking_delta,
                        interrupt_flag,
                    )
                    .await
            }
            LlmClient::OpenAI(client) => {
                let response = client.create_openai_message(request).await?;
                for block in &response.content {
                    if let types::ContentBlock::Text { text } = block {
                        on_text_delta(text);
                    }
                }
                Ok(response)
            }
        }
    }

    pub async fn check_connectivity(&self) -> crate::error::Result<()> {
        match self {
            LlmClient::Anthropic(client) => client.check_connectivity().await,
            LlmClient::OpenAI(client) => client.check_connectivity().await,
        }
    }

    pub fn provider_name(&self) -> &'static str {
        match self {
            LlmClient::Anthropic(_) => "Anthropic",
            LlmClient::OpenAI(_) => "OpenAI",
        }
    }

    #[allow(dead_code)]
    pub fn is_anthropic(&self) -> bool {
        matches!(self, LlmClient::Anthropic(_))
    }
}
