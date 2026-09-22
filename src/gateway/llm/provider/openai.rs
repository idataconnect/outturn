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
            client: crate::http_client::streaming_client(crate::http_client::IDLE_TIMEOUT),
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
        ProviderError::from_response(response).await
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
            .map_err(|e| ProviderError::transport(&e))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::transport(&e))
    }

    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let mut streaming = request.clone();
        streaming.stream = true;
        // Asked for explicitly, because a streamed response omits usage
        // otherwise -- and a turn that cannot say what it cost is a turn
        // nobody can be billed for. The final chunk carries it with an empty
        // `choices`, which readers of the stream already tolerate.
        streaming.stream_options = Some(crate::gateway::llm::types::StreamOptions {
            include_usage: true,
        });

        let response = self
            .request("/v1/chat/completions")
            .json(&streaming)
            .send()
            .await
            .map_err(|e| ProviderError::transport(&e))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        // Server-Sent Events: `data: {json}` lines, blank-line separated,
        // terminated by `data: [DONE]`. Unwrapped here so no other tier has to
        // know the framing.
        let stream = response
            .bytes_stream()
            .map_err(|e| ProviderError::transport(&e));

        Ok(Box::pin(unwrap_sse(stream)))
    }

    fn endpoint(&self) -> String {
        format!("openai:{}", self.base_url)
    }
}

/// Turns a byte stream of Server-Sent Events into decoded chunks.
///
/// The framing is shared with every other provider that speaks SSE; only the
/// shape behind it is this protocol's own.
fn unwrap_sse<S>(stream: S) -> impl Stream<Item = Result<StreamChunk, ProviderError>>
where
    S: Stream<Item = Result<bytes::Bytes, ProviderError>>,
{
    super::sse_payloads(stream).filter_map(|payload| {
        let decoded = match payload {
            Ok(value) => match serde_json::from_value::<StreamChunk>(value) {
                Ok(chunk) => Some(Ok(chunk)),
                Err(e) => {
                    tracing::warn!(error = %e, "malformed stream chunk");
                    None
                }
            },
            Err(e) => Some(Err(e)),
        };
        std::future::ready(decoded)
    })
}
