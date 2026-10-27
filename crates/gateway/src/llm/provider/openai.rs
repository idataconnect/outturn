//! The OpenAI chat-completions protocol.
//!
//! A protocol rather than a vendor: OpenAI, ollama, Groq, OpenRouter, Together
//! and most others speak it, differing only in base URL, credential and which
//! models they host. Those are configuration, so they do not each need a file.
//! Anthropic is the exception that earns one, because its wire format is
//! genuinely different.

use futures::stream::{BoxStream, Stream, StreamExt, TryStreamExt};

use super::{LlmProvider, Provider, ProviderError};
use outturn_core::llm::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk, Usage};

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
            client: outturn_core::http_client::streaming_client(
                outturn_core::http_client::IDLE_TIMEOUT,
            ),
        }
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
            .map_err(|e| ProviderError::sending(&e))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        let mut response: ChatCompletionResponse =
            response.json().await.map_err(|e| ProviderError::lost(&e))?;
        response.usage = response.usage.map(unfold_cache);
        Ok(response)
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
        streaming.stream_options = Some(outturn_core::llm::StreamOptions {
            include_usage: true,
        });

        let response = self
            .request("/v1/chat/completions")
            .json(&streaming)
            .send()
            .await
            .map_err(|e| ProviderError::sending(&e))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        // Server-Sent Events: `data: {json}` lines, blank-line separated,
        // terminated by `data: [DONE]`. Unwrapped here so no other tier has to
        // know the framing.
        let stream = response.bytes_stream().map_err(|e| ProviderError::lost(&e));

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
                Ok(mut chunk) => {
                    chunk.usage = chunk.usage.map(unfold_cache);
                    Some(Ok(chunk))
                }
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

/// This protocol's usage, in the canonical meaning.
///
/// The shape is already the canonical one, which is exactly why the meaning
/// used to slip past: the protocol counts cached tokens *inside*
/// `prompt_tokens`, and a reader that took the number as it came billed every
/// cached token twice -- once at the cached rate and once at full. Taken out
/// here, where this protocol is spoken, so the runtime is handed the same
/// meaning from every route and has no subtraction of its own to get wrong.
/// The provider's own object rides through untouched in `extra`, as it does
/// for the protocols that are translated.
fn unfold_cache(mut usage: Usage) -> Usage {
    let cached = usage
        .prompt_tokens_details
        .as_ref()
        .map_or(0, |d| d.cached_tokens);
    usage.prompt_tokens = usage.prompt_tokens.saturating_sub(cached);
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same facts every protocol's test states: 80 fresh, 20 read from
    /// cache, 5 written. This protocol says 100 and lets the detail explain.
    #[test]
    fn cached_tokens_come_out_of_the_prompt_count() {
        let usage: Usage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 7,
            "total_tokens": 107,
            "prompt_tokens_details": { "cached_tokens": 20, "cache_creation_tokens": 5 },
        }))
        .expect("usage");
        let usage = unfold_cache(usage);
        assert_eq!(usage.prompt_tokens, 80, "full-rate input only");
        let details = usage.prompt_tokens_details.expect("detail");
        assert_eq!(details.cached_tokens, 20);
        assert_eq!(details.cache_creation_tokens, 5);
        assert_eq!(usage.total_tokens, 107);
    }

    /// An endpoint that reports no breakdown reports no cache, and nothing
    /// changes.
    #[test]
    fn a_prompt_count_without_a_breakdown_is_taken_whole() {
        let usage: Usage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 100, "completion_tokens": 7, "total_tokens": 107,
        }))
        .expect("usage");
        assert_eq!(unfold_cache(usage).prompt_tokens, 100);
    }
}
