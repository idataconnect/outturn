use futures::{StreamExt, TryStreamExt};

use super::{BoxStream, LlmProvider, Provider, ProviderError};
use crate::gateway::llm::translate::{self, GeminiStream};
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

/// Gemini, over its own API rather than its OpenAI-compatible endpoint.
///
/// The compatible endpoint is Google's translation of this API into the other
/// protocol's shape, and that translation happens before anything here sees
/// it: cached and thinking token counts have nowhere to go in the shape it
/// produces, so they are folded into totals or dropped. A bill that has to be
/// explained later cannot be explained from numbers that were discarded
/// upstream, so the native API is dialled and the translating is done here,
/// where what it costs is kept beside what it was turned into.
pub struct GeminiProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            base_url,
            api_key,
            model,
            client: crate::http_client::streaming_client(crate::http_client::IDLE_TIMEOUT),
        }
    }

    /// Gemini names the method on the model segment, after a colon.
    fn url(&self, model: &str, method: &str) -> String {
        format!("{}/v1beta/models/{model}:{method}", self.base_url)
    }

    /// The model a request asks for, falling back to the route's own.
    fn model_for<'a>(&'a self, request: &'a ChatCompletionRequest) -> &'a str {
        if request.model.is_empty() {
            &self.model
        } else {
            &request.model
        }
    }

    async fn fail(response: reqwest::Response) -> ProviderError {
        let status = response.status();
        if status.as_u16() == 429 {
            return ProviderError::RateLimited;
        }
        let body = response.text().await.unwrap_or_default();
        ProviderError::Upstream(format!("{status}: {body}"))
    }
}

#[async_trait::async_trait]
impl LlmProvider for GeminiProvider {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let model = self.model_for(request).to_string();
        let body = translate::openai_to_gemini_for(request, &model)
            .map_err(|e| ProviderError::Translation(e.to_string()))?;

        let response = self
            .client
            .post(self.url(&model, "generateContent"))
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        translate::gemini_to_openai(&value).map_err(|e| ProviderError::Translation(e.to_string()))
    }

    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let model = self.model_for(request).to_string();
        let body = translate::openai_to_gemini_for(request, &model)
            .map_err(|e| ProviderError::Translation(e.to_string()))?;

        let response = self
            .client
            // Without `alt=sse` the streaming endpoint answers with a JSON
            // array delivered in pieces, which is not a framing anything else
            // here understands.
            .post(format!(
                "{}?alt=sse",
                self.url(&model, "streamGenerateContent")
            ))
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::fail(response).await);
        }

        let bytes = response
            .bytes_stream()
            .map_err(|e| ProviderError::Upstream(e.to_string()));

        let mut state = GeminiStream::new(model);
        Ok(Box::pin(super::sse_payloads(bytes).filter_map(
            move |event| {
                let chunk = match event {
                    Ok(value) => state.event(&value).map(Ok),
                    Err(e) => Some(Err(e)),
                };
                std::future::ready(chunk)
            },
        )))
    }

    fn endpoint(&self) -> String {
        format!("gemini:{}", self.base_url)
    }
}
