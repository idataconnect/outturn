//! The OpenAI chat-completions protocol.
//!
//! A protocol rather than a vendor: OpenAI, ollama, Groq, OpenRouter, Together
//! and most others speak it, differing only in base URL, credential and which
//! models they host. Those are configuration, so they do not each need a file.
//! Anthropic is the exception that earns one, because its wire format is
//! genuinely different.

use futures::stream::{BoxStream, Stream, StreamExt, TryStreamExt};

use super::{LlmProvider, Provider, ProviderError};
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

pub struct OpenAiProvider {
    base_url: String,
    /// Absent for endpoints that want no credential, such as a local ollama.
    /// Sending an empty bearer token makes some of them reject the request.
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            // Trailing slashes would produce `//v1/...`, which some gateways
            // route differently and others reject outright.
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Configured by base URL, so pointing at a local runtime is the same
    /// operation as pointing at a hosted one.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("OPENAI_BASE_URL").ok()?;
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty());
        Some(Self::new(base_url, api_key))
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let builder = self.client.post(format!("{}{path}", self.base_url));
        match &self.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        }
    }

    async fn fail(response: reqwest::Response) -> ProviderError {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            return ProviderError::RateLimited;
        }
        ProviderError::Upstream(format!("{status}: {body}"))
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let response = self
            .request("/v1/chat/completions")
            .json(request)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))
    }

    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let mut streaming = request.clone();
        streaming.stream = true;

        let response = self
            .request("/v1/chat/completions")
            .json(&streaming)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        // Server-Sent Events: `data: {json}` lines, blank-line separated,
        // terminated by `data: [DONE]`. Unwrapped here so no other tier has to
        // know the framing.
        let stream = response
            .bytes_stream()
            .map_err(|e| ProviderError::Upstream(e.to_string()));

        Ok(Box::pin(unwrap_sse(stream)))
    }

    async fn is_available(&self) -> bool {
        // No cheap health check is common to every endpoint speaking this
        // protocol, so availability is assumed and a failure falls through to
        // the next provider. One wasted request when a local runtime is down,
        // against not having to teach this file about each vendor's probe.
        true
    }
}

/// Turns a byte stream of Server-Sent Events into decoded chunks.
///
/// Bytes arrive without regard for line boundaries, so a partial line is
/// carried over rather than parsed: splitting on whatever a packet happened to
/// contain would corrupt any chunk spanning two reads.
fn unwrap_sse<S>(stream: S) -> impl Stream<Item = Result<StreamChunk, ProviderError>>
where
    S: Stream<Item = Result<bytes::Bytes, ProviderError>>,
{
    let buffer = String::new();
    futures::stream::unfold(
        (Box::pin(stream), buffer, false),
        |(mut stream, mut buffer, mut done)| async move {
            loop {
                if done {
                    return None;
                }

                // Emit anything already buffered before reading more.
                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim().to_string();
                    buffer.drain(..=index);

                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let payload = payload.trim();

                    if payload == "[DONE]" {
                        return None;
                    }
                    if payload.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<StreamChunk>(payload) {
                        Ok(chunk) => return Some((Ok(chunk), (stream, buffer, done))),
                        Err(e) => {
                            tracing::warn!(error = %e, "malformed stream chunk");
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
                    // Stream ended without [DONE]; emit nothing further.
                    None => done = true,
                }
            }
        },
    )
}
