/// Configuration for the language model
#[derive(Clone)]
pub struct ModelConfig {
    pub model: String,
    pub max_tokens: u32,
    pub enable_thinking: bool,
    pub thinking_budget: u32,
}

impl ModelConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        enable_thinking: bool,
        thinking_budget: u32,
    ) -> Self {
        Self {
            model,
            max_tokens,
            enable_thinking,
            thinking_budget,
        }
    }

    pub fn set_thinking(&mut self, enabled: bool) {
        self.enable_thinking = enabled;
    }
}
