use std::sync::Arc;

use wasmtime::{Caller, Linker};

use super::sandbox::SandboxState;

/// Called by the host as generation proceeds.
///
/// The guest never sees deltas: it would have to re-parse a growing string on
/// every one and guard against acting on an unfinished sentence, which is a
/// hazard in every guest for a capability almost none of them want. The host
/// accumulates and reports progress instead, which also means cancellation is
/// simply dropping the stream rather than something threaded through the ABI.
pub type DeltaSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Host-side context for the guest's LLM calls.
///
/// The token and URL live here rather than in guest memory: the guest names a
/// request and receives a reply, and never handles the credential that
/// authorises it. A compromised guest can therefore spend the session's
/// allowance, but cannot take the token elsewhere.
#[derive(Clone)]
pub struct LlmContext {
    pub gateway_url: String,
    pub gateway_token: String,
    pub http: reqwest::Client,
    /// Set when a call has produced a reply the host wants the guest to read.
    pub last_response: Arc<std::sync::Mutex<Option<String>>>,
    /// Receives text as it arrives, when the caller wants progress.
    pub on_delta: Option<DeltaSink>,
}

impl LlmContext {
    pub fn new(gateway_url: String, gateway_token: String) -> Self {
        Self {
            gateway_url,
            gateway_token,
            http: reqwest::Client::new(),
            last_response: Arc::new(std::sync::Mutex::new(None)),
            on_delta: None,
        }
    }

    pub fn with_delta_sink(mut self, sink: DeltaSink) -> Self {
        self.on_delta = Some(sink);
        self
    }
}

fn read_guest_string(
    caller: &mut Caller<'_, SandboxState>,
    ptr: i32,
    len: i32,
) -> Option<String> {
    let memory = caller.get_export("memory")?.into_memory()?;
    let data = memory.data(caller);
    let start = ptr as usize;
    let end = start.checked_add(len as usize)?;
    // Bounds are checked rather than trusted: the guest supplies both.
    let bytes = data.get(start..end)?;
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

/// Links `llm_chat` and `llm_response_read`.
///
/// Two calls rather than one because a guest cannot receive a variable-length
/// result in a single call: it asks for the completion, is told how many bytes
/// came back, then reads them into a buffer it has sized itself.
pub fn link(linker: &mut Linker<SandboxState>, ctx: LlmContext) -> anyhow::Result<()> {
    let call_ctx = ctx.clone();
    linker.func_wrap(
        "env",
        "llm_chat",
        move |mut caller: Caller<'_, SandboxState>, ptr: i32, len: i32| -> i64 {
            let Some(request) = read_guest_string(&mut caller, ptr, len) else {
                return -1;
            };

            let ctx = call_ctx.clone();
            let body = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(stream_completion(&ctx, request))
            });

            match body {
                Ok(body) => {
                    let len = body.len() as i64;
                    *call_ctx.last_response.lock().unwrap() = Some(body);
                    len
                }
                Err(e) => {
                    tracing::warn!(error = %e, "guest LLM call failed");
                    -1
                }
            }
        },
    )?;

    let read_ctx = ctx;
    linker.func_wrap(
        "env",
        "llm_response_read",
        move |mut caller: Caller<'_, SandboxState>, buf_ptr: i32, buf_len: i32| -> i32 {
            // Taken rather than copied: a response is read once, so a guest
            // cannot re-read a buffer the host has moved on from.
            let Some(body) = read_ctx.last_response.lock().unwrap().take() else {
                return -1;
            };

            let bytes = body.as_bytes();
            if bytes.len() > buf_len as usize {
                return -1;
            }

            let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                return -1;
            };
            match memory.write(&mut caller, buf_ptr as usize, bytes) {
                Ok(()) => bytes.len() as i32,
                Err(_) => -1,
            }
        },
    )?;

    Ok(())
}

/// Issues the completion, reporting text as it arrives and returning the whole
/// response once generation ends.
///
/// The gateway answers in newline-delimited JSON; each line is a chunk whose
/// delta carries a fragment of the reply. Assembling them here is what lets the
/// guest stay synchronous while the browser still sees tokens appear.
async fn stream_completion(
    ctx: &LlmContext,
    request: String,
) -> anyhow::Result<String> {
    use futures::StreamExt;

    let response = ctx
        .http
        .post(format!("{}/v1/chat/completions/stream", ctx.gateway_url))
        .bearer_auth(&ctx.gateway_token)
        .header("content-type", "application/json")
        .body(request)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("gateway returned {status}: {body}");
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut accumulated = String::new();
    let mut last: Option<serde_json::Value> = None;

    while let Some(bytes) = stream.next().await {
        buffer.push_str(std::str::from_utf8(&bytes?)?);

        // A chunk may span reads, so only whole lines are parsed.
        while let Some(index) = buffer.find('\n') {
            let line = buffer[..index].trim().to_string();
            buffer.drain(..=index);
            if line.is_empty() {
                continue;
            }

            let chunk: serde_json::Value = match serde_json::from_str(&line) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "malformed chunk from gateway");
                    continue;
                }
            };

            if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str()
                && !text.is_empty()
            {
                accumulated.push_str(text);
                if let Some(sink) = &ctx.on_delta {
                    sink(text);
                }
            }
            last = Some(chunk);
        }
    }

    // Rebuild a non-streaming response so the guest sees one shape regardless
    // of how it was produced.
    let template = last.unwrap_or_else(|| serde_json::json!({}));
    Ok(serde_json::json!({
        "id": template["id"],
        "object": "chat.completion",
        "created": template["created"],
        "model": template["model"],
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": accumulated },
            "finish_reason": template["choices"][0]["finish_reason"],
        }],
        "usage": template["usage"],
    })
    .to_string())
}
