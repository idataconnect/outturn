//! Claude on Amazon Bedrock, through its Invoke API.
//!
//! Bedrock offers two ways in. Converse is one request shape for every model
//! Bedrock hosts, with its own rules for caching and thinking. Invoke takes the
//! model's own format -- for Claude, the Anthropic Messages body -- so the same
//! model behaves the same through every door, and a fallback from Bedrock to
//! Anthropic direct is genuinely the same model. That is the one this is: the
//! body is what `translate::openai_to_anthropic` builds, with the two fields
//! Bedrock wants differently, and the events in the stream are Anthropic's.
//!
//! What differs is the door. The model is named in the path, not the body; a
//! request is authenticated with a Bedrock API key as a bearer token, or signed
//! with AWS keys (SigV4, `super::sigv4`); and a streamed response is AWS's
//! binary event stream, each frame carrying one Anthropic event,
//! base64-encoded.

use futures::{StreamExt, TryStreamExt};

use super::{BoxStream, LlmProvider, Provider, ProviderError};
use crate::gateway::llm::translate::{self, AnthropicStream};
use crate::llm::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

/// The Messages version Bedrock speaks, sent in the body rather than as a
/// header.
const BEDROCK_ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";

/// How a request proves who sent it.
pub enum Auth {
    /// A Bedrock API key, as a bearer token.
    ApiKey(String),
    /// AWS keys, signing each request for a region.
    Signed {
        credentials: super::sigv4::Credentials,
        region: String,
    },
}

impl Auth {
    /// The keys AWS's own tools read, from the gateway's environment, signing
    /// for the endpoint's region: from its host (`bedrock-runtime.<region>.
    /// amazonaws.com`), else `AWS_REGION`, which a local emulator's host does
    /// not carry. None when no keys are set.
    pub fn from_aws_environment(base_url: &str) -> Option<Self> {
        let var = |name| {
            std::env::var(name)
                .ok()
                .filter(|v: &String| !v.trim().is_empty())
        };
        let credentials = super::sigv4::Credentials {
            access_key_id: var("AWS_ACCESS_KEY_ID")?,
            secret_access_key: var("AWS_SECRET_ACCESS_KEY")?,
            session_token: var("AWS_SESSION_TOKEN"),
        };
        let region = region_of(base_url)
            .or_else(|| var("AWS_REGION"))
            .unwrap_or_else(|| "us-east-1".to_string());
        Some(Self::Signed {
            credentials,
            region,
        })
    }
}

/// The region in a Bedrock endpoint's host, where there is one.
fn region_of(base_url: &str) -> Option<String> {
    let host = reqwest::Url::parse(base_url).ok()?.host_str()?.to_string();
    let rest = host.strip_prefix("bedrock-runtime.")?;
    let region = rest.split('.').next()?;
    (!region.is_empty() && rest.contains("amazonaws.com")).then(|| region.to_string())
}

pub struct BedrockProvider {
    /// `https://bedrock-runtime.<region>.amazonaws.com`.
    pub base_url: String,
    auth: Auth,
    client: reqwest::Client,
}

impl BedrockProvider {
    pub fn new(base_url: String, auth: Auth) -> Self {
        Self {
            base_url,
            auth,
            client: crate::http_client::streaming_client(crate::http_client::IDLE_TIMEOUT),
        }
    }

    /// A POST of `body` to `url`, authenticated. Signed requests sign the body
    /// as it will be sent, so it is serialized once, here, and sent as bytes.
    fn post(&self, url: &str, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let bytes = serde_json::to_vec(body).unwrap_or_default();
        let request = self
            .client
            .post(url)
            .header("content-type", "application/json");
        let request = match &self.auth {
            Auth::ApiKey(key) => request.bearer_auth(key),
            Auth::Signed {
                credentials,
                region,
            } => {
                let parsed = reqwest::Url::parse(url).ok();
                let host = parsed
                    .as_ref()
                    .and_then(|u| {
                        u.host_str().map(|h| match u.port() {
                            Some(port) => format!("{h}:{port}"),
                            None => h.to_string(),
                        })
                    })
                    .unwrap_or_default();
                let path = parsed
                    .as_ref()
                    .map(|u| u.path().to_string())
                    .unwrap_or_default();
                let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
                let signed = super::sigv4::sign(
                    "POST",
                    &host,
                    &path,
                    "",
                    &bytes,
                    credentials,
                    region,
                    "bedrock",
                    &amz_date,
                );
                signed
                    .headers
                    .into_iter()
                    .fold(request, |request, (name, value)| {
                        request.header(name, value)
                    })
            }
        };
        request.body(bytes)
    }

    /// Where a model is invoked. The id goes in the path, escaped: Bedrock's
    /// ids carry a colon (`...-v1:0`), and an inference profile a region
    /// prefix (`us.anthropic.claude-...`).
    fn url(&self, model: &str, streaming: bool) -> String {
        let action = if streaming {
            "invoke-with-response-stream"
        } else {
            "invoke"
        };
        let model: String = model
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect();
        format!(
            "{}/model/{model}/{action}",
            self.base_url.trim_end_matches('/')
        )
    }

    fn body(request: &ChatCompletionRequest) -> Result<serde_json::Value, ProviderError> {
        let mut body = translate::openai_to_anthropic(request)
            .map_err(|e| ProviderError::Translation(e.to_string()))?;
        to_bedrock(&mut body);
        Ok(body)
    }
}

/// The Anthropic body, as Bedrock wants it: the model in the path rather than
/// here, streaming chosen by the endpoint rather than a flag, and the version
/// in the body rather than a header.
fn to_bedrock(body: &mut serde_json::Value) {
    if let Some(fields) = body.as_object_mut() {
        fields.remove("model");
        fields.remove("stream");
        fields.insert(
            "anthropic_version".into(),
            serde_json::json!(BEDROCK_ANTHROPIC_VERSION),
        );
    }
}

#[async_trait::async_trait]
impl LlmProvider for BedrockProvider {
    fn provider(&self) -> Provider {
        Provider::Bedrock
    }

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let resp = self
            .post(&self.url(&request.model, false), &Self::body(request)?)
            .send()
            .await
            .map_err(|e| ProviderError::transport(&e))?;

        if !resp.status().is_success() {
            return Err(ProviderError::from_response(resp).await);
        }

        // The same response Anthropic sends, so the same translation reads it.
        let anthropic_resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::transport(&e))?;
        translate::anthropic_to_openai(&anthropic_resp)
            .map_err(|e| ProviderError::Translation(e.to_string()))
    }

    async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let response = self
            .post(&self.url(&request.model, true), &Self::body(request)?)
            .header("accept", "application/vnd.amazon.eventstream")
            .send()
            .await
            .map_err(|e| ProviderError::transport(&e))?;

        if !response.status().is_success() {
            return Err(ProviderError::from_response(response).await);
        }

        let bytes = response
            .bytes_stream()
            .map_err(|e| ProviderError::transport(&e));

        // Anthropic's own events once unwrapped, so Anthropic's state machine
        // reads them: the translation is not written twice.
        let mut state = AnthropicStream::new();
        Ok(Box::pin(anthropic_events(bytes).filter_map(move |event| {
            let chunk = match event {
                Ok(value) => state.event(&value).map(Ok),
                Err(e) => Some(Err(e)),
            };
            std::future::ready(chunk)
        })))
    }

    fn endpoint(&self) -> String {
        format!("bedrock:{}", self.base_url)
    }
}

/// One frame of AWS's event stream: its headers that matter here, and its
/// payload.
#[derive(Debug, PartialEq)]
struct Frame {
    message_type: Option<String>,
    event_type: Option<String>,
    exception_type: Option<String>,
    payload: Vec<u8>,
}

/// Why a frame could not be read. The stream is binary and checksummed, so a
/// frame that does not add up is the transport failing, not something to skip.
fn malformed(what: &str) -> ProviderError {
    ProviderError::transport(format!("malformed event stream: {what}"))
}

/// Reads one frame from the front of `buf`, if a whole one has arrived.
///
/// The layout: a prelude of total length, header length and a checksum of
/// those eight bytes; the headers; the payload; and a checksum of everything
/// before it. Bytes arrive without regard for frame boundaries, so a partial
/// frame is left in the buffer for the next read.
fn take_frame(buf: &mut Vec<u8>) -> Result<Option<Frame>, ProviderError> {
    const PRELUDE: usize = 12;
    if buf.len() < PRELUDE {
        return Ok(None);
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let total = be32(&buf[0..4]) as usize;
    let headers_len = be32(&buf[4..8]) as usize;
    if crc32fast::hash(&buf[0..8]) != be32(&buf[8..12]) {
        return Err(malformed("prelude checksum"));
    }
    // A frame is at least its prelude, its headers and the closing checksum.
    if total < PRELUDE + headers_len + 4 {
        return Err(malformed("frame shorter than its headers"));
    }
    if buf.len() < total {
        return Ok(None);
    }
    if crc32fast::hash(&buf[..total - 4]) != be32(&buf[total - 4..total]) {
        return Err(malformed("message checksum"));
    }

    let mut frame = Frame {
        message_type: None,
        event_type: None,
        exception_type: None,
        payload: buf[PRELUDE + headers_len..total - 4].to_vec(),
    };
    let mut headers = &buf[PRELUDE..PRELUDE + headers_len];
    while !headers.is_empty() {
        let name_len = headers[0] as usize;
        let name = headers
            .get(1..1 + name_len)
            .ok_or_else(|| malformed("header name"))?;
        let name = String::from_utf8_lossy(name).to_string();
        let rest = &headers[1 + name_len..];
        let kind = *rest.first().ok_or_else(|| malformed("header type"))?;
        let rest = &rest[1..];
        // Every type's size, so a header this does not read is stepped over
        // rather than misread as the start of the next.
        let (value, used) = match kind {
            0 | 1 => (None, 0),
            2 => (None, 1),
            3 => (None, 2),
            4 => (None, 4),
            5 | 8 => (None, 8),
            9 => (None, 16),
            6 | 7 => {
                let len = rest
                    .get(0..2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                    .ok_or_else(|| malformed("header length"))?;
                let bytes = rest
                    .get(2..2 + len)
                    .ok_or_else(|| malformed("header value"))?;
                let value = (kind == 7).then(|| String::from_utf8_lossy(bytes).to_string());
                (value, 2 + len)
            }
            _ => return Err(malformed("header type")),
        };
        if rest.len() < used {
            return Err(malformed("header value"));
        }
        match name.as_str() {
            ":message-type" => frame.message_type = value,
            ":event-type" => frame.event_type = value,
            ":exception-type" => frame.exception_type = value,
            _ => {}
        }
        headers = &rest[used..];
    }

    buf.drain(..total);
    Ok(Some(frame))
}

/// What a frame means: an Anthropic event, nothing, or the failure it reports.
///
/// An event's payload is `{"bytes": "<base64 of the event's JSON>"}`. An
/// exception arrives as a frame of its own, partway through, and is mapped to
/// the failure it is: throttling is the endpoint asking to be retried, a
/// validation error is the request at fault, anything else is the model or
/// the service failing.
fn event_of(frame: Frame) -> Option<Result<serde_json::Value, ProviderError>> {
    use base64::Engine as _;
    let detail = || String::from_utf8_lossy(&frame.payload).to_string();
    match frame.message_type.as_deref() {
        Some("event") if frame.event_type.as_deref() == Some("chunk") => {
            let wrapper: serde_json::Value = match serde_json::from_slice(&frame.payload) {
                Ok(v) => v,
                Err(_) => return Some(Err(malformed("chunk payload"))),
            };
            let encoded = wrapper["bytes"].as_str()?;
            let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
                Ok(d) => d,
                Err(_) => return Some(Err(malformed("chunk encoding"))),
            };
            Some(serde_json::from_slice(&decoded).map_err(|_| malformed("chunk event")))
        }
        Some("exception") | Some("error") => {
            let kind = frame
                .exception_type
                .clone()
                .or(frame.event_type.clone())
                .unwrap_or_default();
            Some(Err(match kind.as_str() {
                "throttlingException" => ProviderError::RateLimited,
                "validationException" => ProviderError::Upstream {
                    status: Some(400),
                    detail: format!("400 Bad Request: {}", detail()),
                },
                _ => ProviderError::Upstream {
                    status: Some(500),
                    detail: format!("500 {kind}: {}", detail()),
                },
            }))
        }
        // Anything else -- an initial-response frame, a new event type -- is
        // nothing to forward.
        _ => None,
    }
}

/// The Anthropic events in a byte stream of AWS frames.
fn anthropic_events<S>(
    stream: S,
) -> impl futures::Stream<Item = Result<serde_json::Value, ProviderError>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send + 'static,
{
    futures::stream::unfold(
        (Box::pin(stream), Vec::new(), false),
        |(mut stream, mut buf, mut done)| async move {
            loop {
                if done {
                    return None;
                }
                match take_frame(&mut buf) {
                    Ok(Some(frame)) => {
                        if let Some(event) = event_of(frame) {
                            // A failure ends the stream: nothing after it is
                            // the same response.
                            done = event.is_err();
                            return Some((event, (stream, buf, done)));
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => return Some((Err(e), (stream, buf, true))),
                }
                match stream.next().await {
                    Some(Ok(bytes)) => buf.extend_from_slice(&bytes),
                    Some(Err(e)) => return Some((Err(e), (stream, buf, true))),
                    None if buf.is_empty() => return None,
                    None => {
                        return Some((
                            Err(malformed("stream ended mid-frame")),
                            (stream, buf, true),
                        ));
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// A frame built the way AWS builds one, with string headers.
    fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();
        for (name, value) in headers {
            encoded.push(name.len() as u8);
            encoded.extend_from_slice(name.as_bytes());
            encoded.push(7);
            encoded.extend_from_slice(&(value.len() as u16).to_be_bytes());
            encoded.extend_from_slice(value.as_bytes());
        }
        let total = 12 + encoded.len() + payload.len() + 4;
        let mut out = Vec::new();
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        let prelude_crc = crc32fast::hash(&out);
        out.extend_from_slice(&prelude_crc.to_be_bytes());
        out.extend_from_slice(&encoded);
        out.extend_from_slice(payload);
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_be_bytes());
        out
    }

    /// An Anthropic event, wrapped as Bedrock sends it.
    fn chunk(event: serde_json::Value) -> Vec<u8> {
        let bytes = base64::engine::general_purpose::STANDARD.encode(event.to_string());
        frame(
            &[
                (":message-type", "event"),
                (":event-type", "chunk"),
                (":content-type", "application/json"),
            ],
            serde_json::json!({ "bytes": bytes }).to_string().as_bytes(),
        )
    }

    async fn events(parts: Vec<Vec<u8>>) -> Vec<Result<serde_json::Value, String>> {
        let stream = futures::stream::iter(
            parts
                .into_iter()
                .map(|p| Ok::<_, ProviderError>(bytes::Bytes::from(p))),
        );
        anthropic_events(stream)
            .map(|e| e.map_err(|e| e.to_string()))
            .collect()
            .await
    }

    /// Bedrock's body is Anthropic's, with the model moved to the path and
    /// the version moved into the body.
    #[test]
    fn the_body_is_anthropics_without_the_model() {
        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "us.anthropic.claude-sonnet-5-5",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }))
        .expect("request");
        let body = BedrockProvider::body(&request).expect("body");
        assert!(body.get("model").is_none(), "{body}");
        assert!(body.get("stream").is_none(), "{body}");
        assert_eq!(body["anthropic_version"], BEDROCK_ANTHROPIC_VERSION);
        assert_eq!(body["messages"][0]["role"], "user");
        // The same model's traits, read through Bedrock's longer id.
        assert_eq!(body["max_tokens"], 128_000);
    }

    #[test]
    fn a_model_id_is_escaped_into_the_path() {
        let p = BedrockProvider::new(
            "https://bedrock-runtime.us-east-1.amazonaws.com/".into(),
            Auth::ApiKey("k".into()),
        );
        assert_eq!(
            p.url("anthropic.claude-sonnet-4-5-20250929-v1:0", true),
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/\
             anthropic.claude-sonnet-4-5-20250929-v1%3A0/invoke-with-response-stream"
        );
    }

    /// Frames split across reads, and two in one read, come out whole and in
    /// order -- a decoder that parsed whatever a read held would corrupt both.
    #[tokio::test]
    async fn frames_are_reassembled_across_reads() {
        let first = chunk(serde_json::json!({"type": "message_start", "message": {}}));
        let second = chunk(serde_json::json!({"type": "message_stop"}));
        let mut joined = first.clone();
        joined.extend_from_slice(&second);
        let (a, b) = joined.split_at(first.len() - 5);

        let got = events(vec![a[..7].to_vec(), a[7..].to_vec(), b.to_vec()]).await;
        let types: Vec<_> = got
            .iter()
            .map(|e| {
                e.as_ref().expect("event")["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(types, ["message_start", "message_stop"]);
    }

    /// An exception partway through is the failure it names, and ends the
    /// stream.
    #[tokio::test]
    async fn an_exception_is_a_failure_of_its_own_kind() {
        let got = events(vec![
            chunk(serde_json::json!({"type": "message_start", "message": {}})),
            frame(
                &[
                    (":message-type", "exception"),
                    (":exception-type", "throttlingException"),
                ],
                br#"{"message":"Too many requests"}"#,
            ),
            chunk(serde_json::json!({"type": "message_stop"})),
        ])
        .await;
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(
            got[1].as_ref().unwrap_err(),
            "the model provider is rate limiting these requests"
        );

        let refused = events(vec![frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "validationException"),
            ],
            br#"{"message":"temperature is not supported"}"#,
        )])
        .await;
        assert_eq!(
            refused[0].as_ref().unwrap_err(),
            "the model provider refused the request: temperature is not supported"
        );
    }

    /// The whole door, against a stand-in for Bedrock on a local socket: the
    /// key as a bearer token, the model in the path, Bedrock's body, and a
    /// streamed reply that comes out as the text the model wrote.
    #[tokio::test]
    async fn a_streamed_reply_comes_through_the_door() {
        use axum::{Router, body::Bytes, extract::Path, http::HeaderMap, routing::post};
        use std::sync::{Arc, Mutex};

        let seen: Arc<Mutex<Option<(String, String, serde_json::Value)>>> = Arc::default();
        let record = Arc::clone(&seen);
        let app = Router::new().route(
            "/model/{model}/invoke-with-response-stream",
            post(move |Path(model): Path<String>, headers: HeaderMap, body: Bytes| {
                let record = Arc::clone(&record);
                async move {
                    let auth = headers["authorization"].to_str().unwrap_or_default().to_string();
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    *record.lock().unwrap() = Some((model, auth, body));
                    let mut stream = Vec::new();
                    for event in [
                        serde_json::json!({"type": "message_start", "message": {"id": "m", "model": "claude", "usage": {"input_tokens": 3, "output_tokens": 1}}}),
                        serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                        serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello from Bedrock"}}),
                        serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 4}}),
                        serde_json::json!({"type": "message_stop"}),
                    ] {
                        stream.extend(chunk(event));
                    }
                    stream
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });

        let provider =
            BedrockProvider::new(format!("http://{addr}"), Auth::ApiKey("bedrock-key".into()));
        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "us.anthropic.claude-sonnet-5-5",
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .expect("request");
        let chunks: Vec<_> = provider
            .chat_completion_stream(&request)
            .await
            .expect("stream")
            .collect()
            .await;
        let text: String = chunks
            .iter()
            .filter_map(|c| c.as_ref().ok())
            .filter_map(|c| c.choices.first().and_then(|ch| ch.delta.content.clone()))
            .collect();
        assert_eq!(text, "Hello from Bedrock");

        let (model, auth, body) = seen.lock().unwrap().clone().expect("a request arrived");
        assert_eq!(model, "us.anthropic.claude-sonnet-5-5");
        assert_eq!(auth, "Bearer bedrock-key");
        assert_eq!(body["anthropic_version"], BEDROCK_ANTHROPIC_VERSION);
        assert!(body.get("model").is_none(), "{body}");
    }

    #[test]
    fn the_region_comes_from_an_aws_endpoint_and_from_nothing_else() {
        assert_eq!(
            region_of("https://bedrock-runtime.eu-west-2.amazonaws.com").as_deref(),
            Some("eu-west-2")
        );
        assert_eq!(region_of("http://ministack:4566"), None);
        assert_eq!(region_of("https://bedrock-runtime.example.com"), None);
    }

    /// A signed request, checked the way AWS checks one: the stand-in
    /// recomputes the signature from the request exactly as it arrived --
    /// host, path, date and body -- and it must match what was sent.
    #[tokio::test]
    async fn a_signed_request_verifies_as_it_arrives() {
        use axum::{Router, body::Bytes, extract::OriginalUri, http::HeaderMap, routing::post};
        use std::sync::{Arc, Mutex};

        let credentials = crate::gateway::llm::provider::sigv4::Credentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: Some("TOKEN".into()),
        };
        let verified: Arc<Mutex<Option<bool>>> = Arc::default();
        let (record, keys) = (Arc::clone(&verified), credentials.clone());
        let app = Router::new().fallback(post(
            move |OriginalUri(uri): OriginalUri, headers: HeaderMap, body: Bytes| {
                let (record, keys) = (Arc::clone(&record), keys.clone());
                async move {
                    let header = |name: &str| {
                        headers
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string()
                    };
                    let expected = crate::gateway::llm::provider::sigv4::sign(
                        "POST",
                        &header("host"),
                        uri.path(),
                        "",
                        &body,
                        &keys,
                        "eu-west-2",
                        "bedrock",
                        &header("x-amz-date"),
                    );
                    let sent = header("authorization");
                    let matches = expected
                        .headers
                        .iter()
                        .any(|(n, v)| *n == "authorization" && *v == sent);
                    *record.lock().unwrap() =
                        Some(matches && header("x-amz-security-token") == "TOKEN");
                    chunk(serde_json::json!({"type": "message_stop"}))
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });

        let provider = BedrockProvider::new(
            format!("http://{addr}"),
            Auth::Signed {
                credentials,
                region: "eu-west-2".into(),
            },
        );
        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "anthropic.claude-sonnet-4-5-20250929-v1:0",
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .expect("request");
        let _: Vec<_> = provider
            .chat_completion_stream(&request)
            .await
            .expect("stream")
            .collect()
            .await;
        assert_eq!(
            *verified.lock().unwrap(),
            Some(true),
            "the signature did not verify"
        );
    }

    /// A frame that does not add up is the transport failing, said as such,
    /// rather than skipped.
    #[tokio::test]
    async fn a_corrupt_frame_is_a_failure_not_a_skip() {
        let mut bad = chunk(serde_json::json!({"type": "message_stop"}));
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        let got = events(vec![bad]).await;
        assert!(
            got[0].as_ref().unwrap_err().contains("message checksum"),
            "{got:?}"
        );

        let cut = chunk(serde_json::json!({"type": "message_stop"}));
        let got = events(vec![cut[..cut.len() - 3].to_vec()]).await;
        assert!(
            got[0].as_ref().unwrap_err().contains("mid-frame"),
            "{got:?}"
        );
    }
}
