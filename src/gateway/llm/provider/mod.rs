pub mod anthropic;
pub mod mock;
pub mod ollama;
pub mod openai;

use super::types::{ChatCompletionRequest, ChatCompletionResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OpenAi,
    Anthropic,
    Ollama,
    Mock,
}

#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    fn provider(&self) -> Provider;

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError>;

    async fn is_available(&self) -> bool;
}

#[derive(Debug)]
pub enum ProviderError {
    Unavailable,
    RateLimited,
    Upstream(String),
    Translation(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "provider unavailable"),
            Self::RateLimited => write!(f, "rate limited"),
            Self::Upstream(e) => write!(f, "upstream error: {e}"),
            Self::Translation(e) => write!(f, "translation error: {e}"),
        }
    }
}
