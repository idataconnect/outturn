use futures::{StreamExt, TryStreamExt};

use super::{BoxStream, LlmProvider, Provider, ProviderError};
use crate::gateway::llm::translate::{self, AnthropicStream};
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

pub struct AnthropicProvider {
    pub base_url: String,
    pub api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: crate::http_client::streaming_client(crate::http_client::IDLE_TIMEOUT),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    fn provider(&self) -> Provider {
        Provider::Anthropic
    }

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let anthropic_req = translate::openai_to_anthropic(request)
            .map_err(|e| ProviderError::Translation(e.to_string()))?;

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&anthropic_req)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(ProviderError::RateLimited);
            }
            return Err(ProviderError::Upstream(format!("{status}: {body}")));
        }

        let anthropic_resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        translate::anthropic_to_openai(&anthropic_resp)
            .map_err(|e| ProviderError::Translation(e.to_string()))
    }

    /// Streams a completion, translating Anthropic's events as they arrive.
    ///
    /// The default implementation on the trait would call the blocking
    /// endpoint and hand back its answer as a single chunk. That works, but a
    /// turn cannot be stopped partway through a call that has not returned,
    /// and nothing reaches a reader until everything has -- so a reply appears
    /// all at once after a wait rather than as it is written.
    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let mut body = translate::openai_to_anthropic(request)
            .map_err(|e| ProviderError::Translation(e.to_string()))?;
        body["stream"] = serde_json::json!(true);

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(ProviderError::RateLimited);
            }
            return Err(ProviderError::Upstream(format!("{status}: {text}")));
        }

        let bytes = response
            .bytes_stream()
            .map_err(|e| ProviderError::Upstream(e.to_string()));

        // The state machine outlives each event, because what an event means
        // depends on the ones before it.
        let mut state = AnthropicStream::new();
        let events = super::sse_payloads(bytes);
        Ok(Box::pin(events.filter_map(move |event| {
            let chunk = match event {
                Ok(value) => state.event(&value).map(Ok),
                Err(e) => Some(Err(e)),
            };
            std::future::ready(chunk)
        })))
    }

    fn endpoint(&self) -> String {
        format!("anthropic:{}", self.base_url)
    }
}
