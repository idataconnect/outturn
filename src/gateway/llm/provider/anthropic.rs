use super::{LlmProvider, Provider, ProviderError};
use crate::gateway::llm::translate;
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse};

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

    fn endpoint(&self) -> String {
        format!("anthropic:{}", self.base_url)
    }

    async fn is_available(&self) -> bool {
        true
    }
}
