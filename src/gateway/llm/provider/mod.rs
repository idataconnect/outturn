pub mod anthropic;
pub mod mock;
pub mod ollama;
pub mod openai;

use futures::stream::BoxStream;

use super::types::{
    ChatCompletionRequest, ChatCompletionResponse, Delta, MessageContent, StreamChoice, StreamChunk,
};

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

    /// Streams a completion as newline-delimited JSON chunks.
    ///
    /// Providers speak Server-Sent Events; that framing is unwrapped here so it
    /// terminates at the gateway rather than propagating through the rest of
    /// the system, which carries updates over the event feed instead.
    ///
    /// Defaults to a single chunk built from the non-streaming call, so a
    /// provider that cannot stream still works.
    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let response = self.chat_completion(request).await?;
        let chunk = StreamChunk {
            id: response.id,
            object: "chat.completion.chunk".into(),
            created: response.created,
            model: response.model,
            choices: response
                .choices
                .into_iter()
                .map(|c| StreamChoice {
                    index: c.index,
                    delta: Delta {
                        role: Some(c.message.role),
                        content: Some(match c.message.content {
                            MessageContent::Text(t) => t,
                            MessageContent::Parts(_) => String::new(),
                        }),
                        tool_calls: None,
                    },
                    finish_reason: c.finish_reason,
                })
                .collect(),
            usage: response.usage,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

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
