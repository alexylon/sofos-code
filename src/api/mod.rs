pub mod anthropic;
pub mod morph;
pub mod openai;
pub mod types;
pub mod utils;

pub use anthropic::AnthropicClient;
pub use morph::MorphClient;
pub use openai::OpenAIClient;
pub use types::*;

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
}
