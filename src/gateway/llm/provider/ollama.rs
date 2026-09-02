use futures::stream::{BoxStream, Stream, StreamExt, TryStreamExt};

use super::{LlmProvider, Provider, ProviderError};
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

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

    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let mut streaming = request.clone();
        streaming.stream = true;

        let response = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&streaming)
            .send()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream(format!("{status}: {body}")));
        }

        // Server-Sent Events: `data: {json}` lines, blank-line separated,
        // terminated by `data: [DONE]`. Unwrapped here so no other tier has to
        // know the framing.
        let stream = response.bytes_stream().map_err(|e| ProviderError::Upstream(e.to_string()));

        Ok(Box::pin(unwrap_sse(stream)))
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
