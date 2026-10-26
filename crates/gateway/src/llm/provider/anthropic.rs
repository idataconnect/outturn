use futures::{StreamExt, TryStreamExt};

use super::{BoxStream, LlmProvider, Provider, ProviderError};
use crate::llm::translate::{self, AnthropicStream};
use outturn_core::llm::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

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
            client: outturn_core::http_client::streaming_client(
                outturn_core::http_client::IDLE_TIMEOUT,
            ),
        }
    }
}

/// The betas a translated body asks for, moved to the header the API reads
/// them from. The translation writes them into the body because that is where
/// Bedrock reads them; sent here, a field the API does not define is a 400.
/// None asked for means no header at all, rather than an empty one.
fn take_betas(body: &mut serde_json::Value) -> reqwest::header::HeaderMap {
    let betas = body
        .as_object_mut()
        .and_then(|b| b.remove("anthropic_beta"))
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect::<Vec<_>>()
        .join(",");
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(value) = reqwest::header::HeaderValue::from_str(&betas)
        && !betas.is_empty()
    {
        headers.insert("anthropic-beta", value);
    }
    headers
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
        let mut anthropic_req = translate::openai_to_anthropic(request)
            .map_err(|e| ProviderError::Translation(e.to_string()))?;
        let betas = take_betas(&mut anthropic_req);

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .headers(betas)
            .json(&anthropic_req)
            .send()
            .await
            .map_err(|e| ProviderError::sending(&e))?;

        if !resp.status().is_success() {
            return Err(ProviderError::from_response(resp).await);
        }

        let anthropic_resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::transport(&e))?;

        translate::anthropic_to_openai(&anthropic_resp)
            .map_err(|e| ProviderError::transport(format!("the answer could not be read: {e}")))
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
        let betas = take_betas(&mut body);

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .headers(betas)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::sending(&e))?;

        if !response.status().is_success() {
            return Err(ProviderError::from_response(response).await);
        }

        let bytes = response
            .bytes_stream()
            .map_err(|e| ProviderError::transport(&e));

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
