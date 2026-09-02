use super::{LlmProvider, Provider, ProviderError};
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse};

pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Option<Self> {
        std::env::var("OLLAMA_URL").ok().map(|url| Self::new(url))
    }
}

#[async_trait::async_trait]
impl LlmProvider for OllamaProvider {
    fn provider(&self) -> Provider {
        Provider::Ollama
    }

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let resp = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(request)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream(format!("{status}: {body}")));
        }

        resp.json()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))
    }

    async fn is_available(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}
