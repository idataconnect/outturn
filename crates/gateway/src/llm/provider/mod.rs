pub mod anthropic;
pub mod aws_credentials;
pub mod bedrock;
pub mod gemini;
pub mod mock;
pub mod openai;
pub mod sigv4;

use futures::stream::BoxStream;

use super::types::{
    ChatCompletionRequest, ChatCompletionResponse, Delta, MessageContent, StreamChoice, StreamChunk,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The wire protocol a provider speaks, not the vendor behind it.
///
/// Most vendors -- ollama, Groq, OpenRouter, Together -- speak the OpenAI
/// chat-completions protocol and differ only in base URL and credential, which
/// is configuration rather than code.
pub enum Provider {
    OpenAi,
    Anthropic,
    /// Claude on Amazon Bedrock, through Invoke: Anthropic's body and events
    /// behind a different door.
    Bedrock,
    Gemini,
    Mock,
}

#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    fn provider(&self) -> Provider;

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError>;

    /// Streams a completion as newline-delimited JSON chunks.
    ///
    /// Providers speak Server-Sent Events; that framing is unwrapped here so it
    /// terminates at the gateway rather than propagating through the rest of
    /// the system, which carries updates over the event feed instead.
    ///
    /// Defaults to a single chunk built from the non-streaming call, so a
    /// provider that cannot stream still works.
    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let response = self.chat_completion(request).await?;
        let chunk = StreamChunk {
            id: response.id,
            object: "chat.completion.chunk".into(),
            created: response.created,
            model: response.model,
            choices: response
                .choices
                .into_iter()
                .map(|c| StreamChoice {
                    index: c.index,
                    delta: Delta {
                        role: Some(c.message.role),
                        content: Some(match c.message.content {
                            MessageContent::Text(t) => t,
                            MessageContent::Parts(_) => String::new(),
                        }),
                        tool_calls: None,
                        reasoning: None,
                        reasoning_content: None,
                        thinking_block: None,
                        refusal: None,
                        refusal_category: None,
                    },
                    finish_reason: c.finish_reason,
                })
                .collect(),
            usage: response.usage,
            service_tier: None,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    /// Identifies this provider's endpoint for health tracking.
    ///
    /// Protocol and base URL, not just protocol: api.openai.com and a local
    /// ollama both speak OpenAI's format, and one being down says nothing
    /// about the other.
    fn endpoint(&self) -> String;
}

#[derive(Debug)]
pub enum ProviderError {
    Unavailable,
    RateLimited,
    /// The endpoint answered and the answer was a failure, or the transport
    /// failed before there was an answer at all.
    ///
    /// `status` is what it answered with, and `None` is the transport case --
    /// a connection refused, a stream cut mid-body, a timeout. The two are
    /// genuinely different and were once told apart by parsing the status back
    /// out of `detail`, which was formatted as `"{status}: {body}"`. That
    /// worked and was still wrong: the status was known at the point the error
    /// was built and thrown away, so every consumer that needed it had to
    /// reconstruct it from prose, and a provider whose message merely began
    /// with digits would have been read as a status.
    Upstream {
        status: Option<u16>,
        detail: String,
    },
    Translation(String),
}

impl ProviderError {
    /// An upstream failure the request itself caused.
    ///
    /// Sending it again changes nothing, and sending it to a *different*
    /// provider changes nothing either -- which is what makes this worth
    /// distinguishing from a 5xx. 408 and 429 are excluded because they are
    /// the endpoint asking to be retried rather than refusing the request.
    ///
    /// A transport error is not one of these: nothing was answered, so nothing
    /// says the request was at fault.
    pub fn is_client_error(&self) -> bool {
        match self {
            Self::Upstream {
                status: Some(status),
                ..
            } => (400..500).contains(status) && *status != 408 && *status != 429,
            _ => false,
        }
    }

    /// Whether the endpoint answered that the model asked for is not there --
    /// retired, renamed, or not offered to this credential.
    ///
    /// A client error, and still not the request's fault: the next route, on
    /// another model or another provider, may well answer it. Told from a
    /// malformed request by the provider's own words, since every provider
    /// answers both with a 400 or a 404 (docs/models.md, *Nothing stops
    /// working*).
    pub fn is_model_gone(&self) -> bool {
        // A 404 or 410 that is about the model is the model not being there.
        const MISSING: &[&str] = &[
            "not found",
            "not_found",
            "does not exist",
            "could not resolve",
            "retired",
            "deprecated",
            "decommissioned",
            "no longer",
            "end of its life",
        ];
        // A 404 from a router that has no such path says so in its own
        // words, and a base URL with `models` in it echoes the path back.
        // None of these is a provider saying the model is gone, and reading
        // one as that would skip a working model for a day over a typo.
        const ROUTING: &[&str] = &[
            "page not found",
            "route not found",
            "no route",
            "cannot post",
            "cannot get",
            "<html",
        ];
        // A 400 says it only in so many words. "Parameter top_k is
        // deprecated for this model" is a 400 about the request, and reading
        // it as a retirement would skip a working model for a day.
        const SAID_OUTRIGHT: &[&str] = &[
            "model not found",
            "model_not_found",
            "no such model",
            "model identifier is invalid",
            "model version has reached the end of its life",
            "model has been deprecated",
            "model has been retired",
            "model has been decommissioned",
            "model is no longer available",
            "model is no longer supported",
        ];
        let Self::Upstream {
            status: Some(status),
            detail,
        } = self
        else {
            return false;
        };
        let detail = detail.to_lowercase();
        match status {
            404 | 410 => {
                detail.contains("model")
                    && MISSING.iter().any(|said| detail.contains(said))
                    && !ROUTING.iter().any(|said| detail.contains(said))
            }
            400 => SAID_OUTRIGHT.iter().any(|said| detail.contains(said)),
            _ => false,
        }
    }

    /// Whether the model wrote a reply its server could not turn into a tool
    /// call -- in practice, arguments cut off mid-string when a model loops
    /// until it runs out of room.
    ///
    /// Recognized from what llama.cpp's server says, the one that reports it
    /// this way; a provider that says nothing about it is simply never this.
    /// Worth telling apart because it is not the provider failing: the
    /// request was taken and answered, at length, and the answer is unusable.
    pub fn is_malformed_reply(&self) -> bool {
        match self {
            Self::Upstream { detail, .. } => detail.contains("Failed to parse tool call arguments"),
            _ => false,
        }
    }

    /// Builds the error for a response that failed, reading the status from
    /// the response rather than back out of the text.
    ///
    /// 429 becomes `RateLimited` here rather than at each call site: the
    /// status is already in hand, and three providers each testing for it
    /// separately is three places to edit when the retryable set changes --
    /// which `is_client_error` already treats as more than one status.
    pub async fn from_response(response: reqwest::Response) -> Self {
        let status = response.status();
        if status.as_u16() == 429 {
            return Self::RateLimited;
        }
        let detail = response.text().await.unwrap_or_default();
        Self::Upstream {
            status: Some(status.as_u16()),
            detail: format!("{status}: {detail}"),
        }
    }

    /// Builds the transport case, where there is no status to read.
    ///
    /// reqwest ends its errors with ` for url (...)`, and the detail is
    /// shown to the model and stored in the transcript, so the URL goes
    /// here the way it does for an agent's own requests.
    pub fn transport(e: impl std::fmt::Display) -> Self {
        Self::Upstream {
            status: None,
            detail: crate::egress::transport::strip_url(&e.to_string()),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "the model provider is unavailable"),
            Self::RateLimited => write!(f, "the model provider is rate limiting these requests"),
            Self::Upstream { .. } if self.is_malformed_reply() => write!(
                f,
                "the model wrote a tool call its arguments could not be read \
                 from, most likely cut off when the reply ran out of room"
            ),
            // One sentence: who failed, how, and the provider's own reason.
            // It once read "upstream error: 400 Bad Request: {...}", the
            // provider's whole error object, which said the same thing in
            // three layers of somebody else's punctuation. The provider is
            // not named: the protocol is not the vendor, and the host is
            // nothing the person reading this should learn. The gateway's log
            // has both, and the whole body.
            Self::Upstream { status, detail } => {
                let what = match status {
                    Some(400..=499) => "refused the request",
                    Some(_) => "failed",
                    None => "could not be reached",
                };
                let reason = reason_in(detail);
                let reason = reason.as_deref().unwrap_or(detail);
                // Capped, because a provider's error body can carry the whole
                // of what it was sent or wrote: one held a looping
                // seventeen-kilobyte document, shown whole as the reason a
                // turn failed.
                const SHOWN: usize = 300;
                match reason.char_indices().nth(SHOWN) {
                    Some((cut, _)) => write!(
                        f,
                        "the model provider {what}: {}… ({} more bytes in the gateway log)",
                        &reason[..cut],
                        reason.len() - cut
                    ),
                    None => write!(f, "the model provider {what}: {reason}"),
                }
            }
            Self::Translation(e) => write!(f, "translation error: {e}"),
        }
    }
}

/// The provider's own explanation, where its error body has one.
///
/// Anthropic, OpenAI, Gemini and llama.cpp all put it at `error.message`;
/// ollama sends `error` as a bare string, and Bedrock a top-level `message`.
/// The body follows the status line in `detail`, so it is read from the first
/// brace on.
fn reason_in(detail: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(&detail[detail.find('{')?..]).ok()?;
    match &body["error"] {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => body["message"].as_str().map(str::to_string),
        error => error["message"].as_str().map(str::to_string),
    }
}

/// Splits a byte stream of Server-Sent Events into their JSON payloads.
///
/// Bytes arrive without regard for line boundaries, so a partial line is
/// carried over rather than parsed: splitting on whatever a packet happened to
/// contain would corrupt any payload spanning two reads.
///
/// Yields values rather than a decoded type because the providers disagree
/// about what a payload is. One sends fragments of a single shape and stops at
/// a sentinel; another sends a sequence of differently-typed events and stops
/// by ending. What they share is exactly this framing, and no more.
pub fn sse_payloads<S>(
    stream: S,
) -> impl futures::Stream<Item = Result<serde_json::Value, ProviderError>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, ProviderError>>,
{
    use futures::StreamExt;

    futures::stream::unfold(
        // A decoder rides along with the buffer: a chunk can end inside a
        // character, and decoding each on its own failed the turn for it.
        (
            Box::pin(stream),
            String::new(),
            outturn_core::utf8::Decoder::new(),
            false,
        ),
        |(mut stream, mut buffer, mut decoder, mut done)| async move {
            loop {
                if done {
                    return None;
                }

                // Emit anything already buffered before reading more.
                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim().to_string();
                    buffer.drain(..=index);

                    // `event:` lines name what follows, which the payload's
                    // own `type` also says. Skipped rather than read, so a
                    // provider that sends one and a provider that does not are
                    // handled by the same code.
                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let payload = payload.trim();

                    // The OpenAI protocol's sentinel. Anthropic never sends
                    // it and ends by ending, which `None` below handles.
                    if payload == "[DONE]" {
                        return None;
                    }
                    if payload.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<serde_json::Value>(payload) {
                        Ok(value) => return Some((Ok(value), (stream, buffer, decoder, done))),
                        Err(e) => {
                            tracing::warn!(error = %e, "malformed stream payload");
                            continue;
                        }
                    }
                }

                match stream.next().await {
                    Some(Ok(bytes)) => {
                        if let Err(e) = decoder.push(&bytes, &mut buffer) {
                            return Some((
                                Err(ProviderError::transport(e)),
                                (stream, buffer, decoder, true),
                            ));
                        }
                    }
                    Some(Err(e)) => return Some((Err(e), (stream, buffer, decoder, true))),
                    // Ended without a sentinel; nothing further to emit.
                    None => done = true,
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(detail: &str) -> ProviderError {
        ProviderError::Upstream {
            status: Some(500),
            detail: detail.to_string(),
        }
    }

    #[test]
    fn a_transport_error_names_no_host() {
        let e = ProviderError::transport(
            "error sending request for url (https://llm.internal.example.com/v1/chat?key=sk-secret)",
        );
        let shown = e.to_string();
        assert!(!shown.contains("example.com"), "{shown}");
        assert!(!shown.contains("sk-secret"), "{shown}");
    }

    #[test]
    fn a_long_upstream_error_is_cut_short_for_the_caller() {
        let shown = upstream(&"karl ".repeat(4000)).to_string();
        assert!(shown.len() < 450, "{} bytes shown", shown.len());
        assert!(shown.contains("more bytes in the gateway log"), "{shown}");
    }

    #[test]
    fn a_short_upstream_error_is_shown_whole() {
        assert_eq!(
            upstream("500: bad").to_string(),
            "the model provider failed: 500: bad"
        );
    }

    /// The case that prompted the sentence: Sonnet 5.5 refusing a temperature
    /// arrived as the provider's whole error object, three layers deep.
    /// What each provider says when the model is gone, as they say it.
    #[test]
    fn a_model_that_is_gone_is_told_from_a_bad_request() {
        let e = |status: u16, detail: &str| ProviderError::Upstream {
            status: Some(status),
            detail: detail.into(),
        };
        for gone in [
            e(
                404,
                r#"{"type":"error","error":{"type":"not_found_error","message":"model: claude-3-opus"}}"#,
            ),
            e(
                404,
                "The model `gpt-4-32k` does not exist or you do not have access to it. model_not_found",
            ),
            e(
                404,
                "models/gemini-1.0-pro is not found for API version v1beta",
            ),
            e(404, "model 'qwen9' not found"),
            e(
                404,
                "This model version has reached the end of its life. Please refer to the model lifecycle docs.",
            ),
            e(
                400,
                "ValidationException: The provided model identifier is invalid.",
            ),
            e(400, "The model has been deprecated; use a newer one."),
        ] {
            assert!(gone.is_model_gone(), "{gone:?}");
        }
        for not_gone in [
            e(400, "temperature: parameter is deprecated for this model"),
            e(400, "messages: field required"),
            e(404, "route not found"),
            e(404, "404 page not found: /v1/models/chat/completions"),
            e(404, "Cannot POST /v1/models/chat/completions"),
            e(
                404,
                "<html><title>404 Not Found</title><body>/models/v1</body>",
            ),
            e(401, "invalid x-api-key"),
            e(500, "model not found"),
        ] {
            assert!(!not_gone.is_model_gone(), "{not_gone:?}");
        }
    }

    #[test]
    fn a_refusal_says_the_providers_reason_and_nothing_else() {
        let refused = ProviderError::Upstream {
            status: Some(400),
            detail: r#"400 Bad Request: {"type":"error","error":{"type":"invalid_request_error","message":"`temperature` is deprecated for this model."},"request_id":"req_1"}"#.into(),
        };
        assert_eq!(
            refused.to_string(),
            "the model provider refused the request: `temperature` is deprecated for this model."
        );
    }

    /// ollama's error is a bare string rather than an object.
    #[test]
    fn a_bare_string_error_is_read_too() {
        let missing = ProviderError::Upstream {
            status: Some(404),
            detail: r#"404 Not Found: {"error":"model \"qwen9\" not found"}"#.into(),
        };
        assert_eq!(
            missing.to_string(),
            r#"the model provider refused the request: model "qwen9" not found"#
        );
    }

    #[test]
    fn a_transport_failure_says_the_provider_could_not_be_reached() {
        let refused = ProviderError::transport("connection refused");
        assert_eq!(
            refused.to_string(),
            "the model provider could not be reached: connection refused"
        );
    }

    #[test]
    fn a_tool_call_that_would_not_parse_is_a_malformed_reply() {
        let e = upstream(
            r#"500: {"error":{"code":500,"message":"Failed to parse tool call arguments as JSON: ..."}}"#,
        );
        assert!(e.is_malformed_reply());
        assert!(e.to_string().contains("cut off"), "{e}");
        assert!(!upstream("500: out of memory").is_malformed_reply());
    }
}
