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

/// A stream declined before it said anything, and what that cost.
struct Declined {
    category: String,
    usage: serde_json::Value,
}

type Chunks = BoxStream<'static, Result<StreamChunk, ProviderError>>;

impl AnthropicProvider {
    /// Sends a streaming request and reads up to the first thing it says,
    /// so a refusal before any output can be told apart from one after it.
    /// What was read is handed back to go first, ahead of the rest.
    async fn open(
        &self,
        body: &serde_json::Value,
        betas: &reqwest::header::HeaderMap,
        mut state: AnthropicStream,
    ) -> Result<
        (
            Vec<Result<StreamChunk, ProviderError>>,
            Option<Declined>,
            Chunks,
        ),
        ProviderError,
    > {
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .headers(betas.clone())
            .json(body)
            .send()
            .await
            .map_err(|e| ProviderError::transport(&e))?;

        if !response.status().is_success() {
            return Err(ProviderError::from_response(response).await);
        }

        let bytes = response
            .bytes_stream()
            .map_err(|e| ProviderError::transport(&e));
        let mut events = Box::pin(super::sse_payloads(bytes));

        let mut head = Vec::new();
        let mut said = false;
        let mut finished = false;
        while let Some(event) = events.next().await {
            let Ok(value) = event else {
                head.push(event.map(|_| unreachable!()));
                break;
            };
            let Some(chunk) = state.event(&value) else {
                continue;
            };
            let delta = &chunk.choices[0].delta;
            said |= delta.content.as_deref().is_some_and(|c| !c.is_empty())
                || delta.tool_calls.is_some()
                || delta.reasoning.is_some()
                || delta.thinking_block.is_some();
            finished |= chunk.choices[0].finish_reason.is_some();
            head.push(Ok(chunk));
            if said || finished {
                break;
            }
        }

        let declined = (!said && finished)
            .then(|| state.refusal_category().map(str::to_string))
            .flatten()
            .map(|category| Declined {
                category,
                usage: serde_json::json!(state.usage().to_usage()),
            });

        // The state machine outlives each event, because what an event means
        // depends on the ones before it.
        let rest = events.filter_map(move |event| {
            let chunk = match event {
                Ok(value) => state.event(&value).map(Ok),
                Err(e) => Some(Err(e)),
            };
            std::future::ready(chunk)
        });
        Ok((head, declined, Box::pin(rest)))
    }
}

/// Takes every thinking block out of a translated request. True if there was
/// one to take.
fn strip_thinking(body: &mut serde_json::Value) -> bool {
    let mut stripped = false;
    for message in body["messages"].as_array_mut().into_iter().flatten() {
        if let Some(blocks) = message["content"].as_array_mut() {
            let before = blocks.len();
            blocks
                .retain(|b| !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
            stripped |= blocks.len() != before;
        }
    }
    stripped
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
            .map_err(|e| ProviderError::transport(&e))?;

        if !resp.status().is_success() {
            return Err(ProviderError::from_response(resp).await);
        }

        let anthropic_resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::transport(&e))?;

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
        let betas = take_betas(&mut body);
        let (head, refused, rest) = self.open(&body, &betas, AnthropicStream::new()).await?;

        // Sending a model its own earlier reasoning can be declined as an
        // attempt to extract it, when the person then asks why it did what it
        // did -- and the same request without that reasoning is answered. So
        // that one refusal, before anything was said, is asked once more
        // without it. Never quietly: it is logged, and marked on the usage the
        // ledger keeps, with what the declined attempt cost (docs/thinking.md).
        if let Some(refused) = refused
            && refused.category == "reasoning_extraction"
            && strip_thinking(&mut body)
        {
            tracing::warn!(
                model = %request.model,
                "declined as reasoning extraction with earlier thinking in the conversation; \
                 asking again without it"
            );
            let mut state = AnthropicStream::new();
            state.note(serde_json::json!({
                "retried_without_thinking": refused.category,
                "declined_attempt": refused.usage,
            }));
            let (head, _, rest) = self.open(&body, &betas, state).await?;
            return Ok(Box::pin(futures::stream::iter(head).chain(rest)));
        }
        Ok(Box::pin(futures::stream::iter(head).chain(rest)))
    }

    fn endpoint(&self) -> String {
        format!("anthropic:{}", self.base_url)
    }
}
