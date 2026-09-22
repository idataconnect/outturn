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
    /// The endpoint answered and the answer was a failure, or the transport
    /// failed before there was an answer at all.
    ///
    /// `status` is what it answered with, and `None` is the transport case --
    /// a connection refused, a stream cut mid-body, a timeout. The two are
    /// genuinely different and were once told apart by parsing the status back
    /// out of `detail`, which was formatted as `"{status}: {body}"`. That
    /// worked and was still wrong: the status was known at the point the error
    /// was built and thrown away, so every consumer that needed it had to
    /// reconstruct it from prose, and a provider whose message merely began
    /// with digits would have been read as a status.
    Upstream {
        status: Option<u16>,
        detail: String,
    },
    Translation(String),
}

impl ProviderError {
    /// An upstream failure the request itself caused.
    ///
    /// Sending it again changes nothing, and sending it to a *different*
    /// provider changes nothing either -- which is what makes this worth
    /// distinguishing from a 5xx. 408 and 429 are excluded because they are
    /// the endpoint asking to be retried rather than refusing the request.
    ///
    /// A transport error is not one of these: nothing was answered, so nothing
    /// says the request was at fault.
    pub fn is_client_error(&self) -> bool {
        match self {
            Self::Upstream {
                status: Some(status),
                ..
            } => (400..500).contains(status) && *status != 408 && *status != 429,
            _ => false,
        }
    }

    /// Builds the error for a response that failed, reading the status from
    /// the response rather than back out of the text.
    ///
    /// 429 becomes `RateLimited` here rather than at each call site: the
    /// status is already in hand, and three providers each testing for it
    /// separately is three places to edit when the retryable set changes --
    /// which `is_client_error` already treats as more than one status.
    pub async fn from_response(response: reqwest::Response) -> Self {
        let status = response.status();
        if status.as_u16() == 429 {
            return Self::RateLimited;
        }
        let detail = response.text().await.unwrap_or_default();
        Self::Upstream {
            status: Some(status.as_u16()),
            detail: format!("{status}: {detail}"),
        }
    }

    /// Builds the transport case, where there is no status to read.
    pub fn transport(e: impl std::fmt::Display) -> Self {
        Self::Upstream {
            status: None,
            detail: e.to_string(),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "provider unavailable"),
            Self::RateLimited => write!(f, "rate limited"),
            Self::Upstream { detail, .. } => write!(f, "upstream error: {detail}"),
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
pub fn sse_payloads<S>(
    stream: S,
) -> impl futures::Stream<Item = Result<serde_json::Value, ProviderError>>
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
                                Err(ProviderError::transport(e)),
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
