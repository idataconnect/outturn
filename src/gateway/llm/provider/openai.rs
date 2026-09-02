use super::{LlmProvider, Provider, ProviderError};
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse};

pub struct OpenAiProvider {
    pub base_url: String,
    pub api_key: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: reqwest::Client::new(),
        }
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
        let resp = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(request)
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

        resp.json()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))
    }

    async fn is_available(&self) -> bool {
        true
    }
}
