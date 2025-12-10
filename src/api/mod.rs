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
            LlmClient::Anthropic(client) => client.create_message(request).await,
            LlmClient::OpenAI(client) => client.create_message(request).await,
        }
    }
}
