pub mod anthropic;
pub mod gemini;
pub mod mock;
pub mod openai;

use futures::stream::BoxStream;

use super::types::{
    ChatCompletionRequest, ChatCompletionResponse, Delta, MessageContent, StreamChoice, StreamChunk,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The wire protocol a provider speaks, not the vendor behind it.
///
/// Most vendors -- ollama, Groq, OpenRouter, Together -- speak the OpenAI
/// chat-completions protocol and differ only in base URL and credential, which
/// is configuration rather than code.
pub enum Provider {
    OpenAi,
    Anthropic,
    Gemini,
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
            service_tier: None,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    /// Identifies this provider's endpoint for health tracking.
    ///
    /// Protocol and base URL, not just protocol: api.openai.com and a local
    /// ollama both speak OpenAI's format, and one being down says nothing
    /// about the other.
    fn endpoint(&self) -> String;
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

/// Splits a byte stream of Server-Sent Events into their JSON payloads.
///
/// Bytes arrive without regard for line boundaries, so a partial line is
/// carried over rather than parsed: splitting on whatever a packet happened to
/// contain would corrupt any payload spanning two reads.
///
/// Yields values rather than a decoded type because the providers disagree
/// about what a payload is. One sends fragments of a single shape and stops at
/// a sentinel; another sends a sequence of differently-typed events and stops
/// by ending. What they share is exactly this framing, and no more.
pub fn sse_payloads<S>(stream: S) -> impl futures::Stream<Item = Result<serde_json::Value, ProviderError>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, ProviderError>>,
{
    use futures::StreamExt;

    futures::stream::unfold(
        (Box::pin(stream), String::new(), false),
        |(mut stream, mut buffer, mut done)| async move {
            loop {
                if done {
                    return None;
                }

                // Emit anything already buffered before reading more.
                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim().to_string();
                    buffer.drain(..=index);

                    // `event:` lines name what follows, which the payload's
                    // own `type` also says. Skipped rather than read, so a
                    // provider that sends one and a provider that does not are
                    // handled by the same code.
                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let payload = payload.trim();

                    // The OpenAI protocol's sentinel. Anthropic never sends
                    // it and ends by ending, which `None` below handles.
                    if payload == "[DONE]" {
                        return None;
                    }
                    if payload.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<serde_json::Value>(payload) {
                        Ok(value) => return Some((Ok(value), (stream, buffer, done))),
                        Err(e) => {
                            tracing::warn!(error = %e, "malformed stream payload");
                            continue;
                        }
                    }
                }

                match stream.next().await {
                    Some(Ok(bytes)) => match std::str::from_utf8(&bytes) {
                        Ok(text) => buffer.push_str(text),
                        Err(e) => {
                            return Some((
                                Err(ProviderError::Upstream(e.to_string())),
                                (stream, buffer, true),
                            ));
                        }
                    },
                    Some(Err(e)) => return Some((Err(e), (stream, buffer, true))),
                    // Ended without a sentinel; nothing further to emit.
                    None => done = true,
                }
            }
        },
    )
}
