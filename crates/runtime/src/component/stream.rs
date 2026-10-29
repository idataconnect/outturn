//! Reading the gateway's stream of a model call: text, thinking, tool
//! calls and usage as they arrive, and what the gateway said beside them.

use super::{
    Arrival, Completion, ContentPart, MalformedReply, ProgressSink, ReasoningSink, Refused, Signed,
    ToolCall, Usage, WritingSink,
};

/// How often a call being written reports how much of it there is.
const WRITING_REPORT_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// The thinking in one chunk, where the provider put any.
///
/// `reasoning` is what ollama and llama.cpp emit; `reasoning_content` is the
/// same thing under the name DeepSeek's API and several of its compatibles use.
/// Both arrive with `content` set to `""` on every thinking chunk, which is why
/// reading `content` alone made a turn spent entirely on thinking look like a
/// turn that produced nothing: the reply stored empty, no delta ever streamed,
/// and the tokens it cost recorded in the ledger with nothing to show for them.
fn thinking_in(chunk: &serde_json::Value) -> Option<&str> {
    let delta = &chunk["choices"][0]["delta"];
    delta["reasoning"]
        .as_str()
        .or_else(|| delta["reasoning_content"].as_str())
        .filter(|t| !t.is_empty())
}

/// Consumes the gateway's stream, reporting text as it arrives and returning
/// the whole reply once generation ends.
pub(super) async fn stream_completion(
    http: &reqwest::Client,
    gateway_url: &str,
    token: &str,
    traffic_type: &str,
    reply_id: &uuid::Uuid,
    body: serde_json::Value,
    progress: Option<&ProgressSink>,
    reasoning: Option<&ReasoningSink>,
    writing: Option<&WritingSink>,
) -> anyhow::Result<(Completion, Vec<(Arrival, Option<uuid::Uuid>)>, Served)> {
    use futures::StreamExt;

    let response = http
        .post(format!("{gateway_url}/v1/chat/completions/stream"))
        .bearer_auth(token)
        .header("x-outturn-traffic", traffic_type)
        // Named so the gateway can record which reply took a message it hands
        // back, rather than only that one was taken.
        .header("x-outturn-reply", reply_id.to_string())
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let malformed = response
            .headers()
            .get(outturn_core::protocol::FAILURE_HEADER)
            .is_some_and(|v| v == outturn_core::protocol::MALFORMED_REPLY);
        let detail = response.text().await.unwrap_or_default();
        if malformed {
            return Err(MalformedReply(detail).into());
        }
        return Err(outturn_core::protocol::gateway_failure(status, &detail));
    }

    // Named by the gateway, so spend attaches to the endpoint that billed
    // for it rather than to whichever one was configured first, and to the
    // credential that paid.
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let mut served = Served {
        cancelled: false,
        held: None,
        endpoint: header("x-outturn-provider"),
        paid_by: header("x-outturn-paid-by"),
        model: None,
        provider_usage: None,
        service_tier: None,
        partial: false,
        usage_estimated: false,
        signed: Signed::default(),
    };
    // What the model was sent and what came back, in characters, in case
    // the provider never says what they cost.
    let sent_chars = body.to_string().chars().count();
    let mut received_chars = 0usize;
    // Signatures on calls, by the call's index, until the calls have ids.
    let mut call_signatures: std::collections::BTreeMap<u32, String> =
        std::collections::BTreeMap::new();

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    // Built in arrival order. A tool call is assembled across several chunks,
    // so its place in the sequence is where it first appeared, not where it
    // finished -- which is what `call_slots` remembers.
    let mut parts: Vec<ContentPart> = Vec::new();
    let mut call_slots: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    // When each call last said how much of it there is.
    let mut reported: std::collections::BTreeMap<u32, std::time::Instant> =
        std::collections::BTreeMap::new();
    let mut finish_reason = None;
    let mut usage = None;
    let mut refused: Option<outturn_core::protocol::work::Refusal> = None;
    // Tool calls arrive in fragments keyed by index, and the arguments are a
    // JSON string spread across chunks. Kept sparse by index rather than
    // pushed, since a provider is free to interleave two calls.
    let mut arrivals: Vec<(Arrival, Option<uuid::Uuid>)> = Vec::new();

    // Chunks are cut wherever the network cut them, which can be inside a
    // character; see `outturn_core::utf8`.
    let mut decoder = outturn_core::utf8::Decoder::new();
    while let Some(bytes) = stream.next().await {
        decoder.push(&bytes?, &mut buffer)?;

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

            // The model that actually answered, which a route may have
            // chosen in place of the one asked for.
            if served.model.is_none()
                && let Some(m) = chunk["model"].as_str()
                && !m.is_empty()
            {
                served.model = Some(m.to_string());
            }
            // Thinking, where a model produces it. Never joined to the reply:
            // it is not what the agent said, the transcript must not replay it
            // to a later turn as prose, and both protocols reject an assistant
            // message whose content is a model's private reasoning.
            //
            // Carried on a channel of its own instead, so the reader can watch
            // a model think without the guest ever seeing it. A turn spent
            // entirely on thinking used to stream nothing at all -- every chunk
            // arrives with `content` set to "" and the thinking under
            // `reasoning`, so the reply was stored empty while the usage ledger
            // recorded the tokens it cost.
            //
            // `reasoning_content` is the same field under the name some
            // OpenAI-compatible endpoints use for it.
            if let Some(thought) = thinking_in(&chunk) {
                received_chars += thought.chars().count();
                if let Some(sink) = reasoning {
                    sink(thought);
                }
            }
            if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str()
                && !text.is_empty()
            {
                received_chars += text.chars().count();
                match parts.last_mut() {
                    Some(ContentPart::Text(t)) => t.push_str(text),
                    _ => parts.push(ContentPart::Text(text.to_string())),
                }
                if let Some(sink) = progress {
                    sink(text);
                }
            }
            // The gateway appends its own line after the provider's chunks,
            // carrying anything the user said while this call was in flight.
            // It rides the response rather than needing a channel of its own,
            // because the runtime holds no credentials and no database.
            if let Some(outturn) = chunk.get("outturn").filter(|o| !o.is_null()) {
                if let Some(pending) = outturn["pending"].as_array() {
                    for message in pending {
                        arrivals.push((
                            Arrival {
                                content: message["content"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                delivery: message["delivery"]
                                    .as_str()
                                    .unwrap_or("steer")
                                    .to_string(),
                            },
                            message["id"].as_str().and_then(|id| id.parse().ok()),
                        ));
                    }
                }
                // Sticky: a cancel is not withdrawn by a later round that does
                // not mention it, and the guest may not ask until the round
                // after the one that carried it.
                if outturn["cancelled"].as_bool() == Some(true) {
                    served.cancelled = true;
                }
                // Why, where a hold rather than a person did it. Carried out
                // of the turn rather than only logged: a hold taken and then
                // released while this turn streamed leaves nothing in the
                // table for the next turn's checkpoint to find, so the latch
                // has to be written from what stopped this one.
                if let Some(reason) = outturn["held"].as_str() {
                    if served.held.is_none() {
                        tracing::info!(reason = %reason, "a hold cut this turn's stream");
                    }
                    served.held = Some(reason.to_string());
                }
                continue;
            }

            if let Some(block) = chunk["choices"][0]["delta"]
                .get("thinking_block")
                .filter(|b| b.is_object())
            {
                served.signed.thinking.push(block.clone());
            }
            if let Some(calls) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"].as_u64().unwrap_or(0) as u32;
                    if let Some(signature) = call["provider_signature"].as_str() {
                        call_signatures.insert(index, signature.to_string());
                    }
                    let slot = *call_slots.entry(index).or_insert_with(|| {
                        parts.push(ContentPart::Call(ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        }));
                        parts.len() - 1
                    });
                    let Some(ContentPart::Call(entry)) = parts.get_mut(slot) else {
                        continue;
                    };
                    if let Some(id) = call["id"].as_str() {
                        entry.id = id.to_string();
                    }
                    if let Some(name) = call["function"]["name"].as_str() {
                        let began = entry.name.is_empty() && !name.is_empty();
                        entry.name.push_str(name);
                        // Said once per call, as soon as it has a name: the
                        // name arrives with its first fragment, and everything
                        // after that is arguments nobody should see half of.
                        if began && let Some(sink) = writing {
                            sink(index, &entry.name, 0);
                            reported.insert(index, std::time::Instant::now());
                        }
                    }
                    if let Some(args) = call["function"]["arguments"].as_str() {
                        received_chars += args.chars().count();
                        entry.arguments.push_str(args);
                        if let Some(sink) = writing
                            && !entry.name.is_empty()
                            && reported
                                .get(&index)
                                .is_none_or(|at| at.elapsed() >= WRITING_REPORT_EVERY)
                        {
                            sink(index, &entry.name, entry.arguments.len() as u64);
                            reported.insert(index, std::time::Instant::now());
                        }
                    }
                }
            }
            if let Some(reason) = chunk["choices"][0]["finish_reason"].as_str() {
                finish_reason = Some(reason.to_string());
            }
            if let Some(why) = chunk["choices"][0]["delta"]["refusal"].as_str() {
                refused = Some(outturn_core::protocol::work::Refusal {
                    reason: why.to_string(),
                    category: chunk["choices"][0]["delta"]["refusal_category"]
                        .as_str()
                        .map(str::to_string),
                    model: None,
                });
            }
            if let Some(tier) = chunk["service_tier"].as_str() {
                served.service_tier = Some(tier.to_string());
            }
            if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
                // Kept whole. The fields read below are the ones every bill
                // needs; the rest are the ones a later bill might.
                served.provider_usage = Some(u.clone());
                usage = Some(Usage {
                    // Taken as the gateway sent it. The gateway defines what
                    // "prompt tokens" means -- the input billed at full rate,
                    // with cached tokens beside it rather than inside -- and
                    // settles every protocol's convention into that one
                    // meaning before it reaches here. Subtracting the cache
                    // again on this side, as this once did on the OpenAI
                    // protocol's account, billed a cached Anthropic or Gemini
                    // conversation at a fraction of its cost.
                    prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                    completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                    cache_read_tokens: u["prompt_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32,
                    cache_write_tokens: u["prompt_tokens_details"]["cache_creation_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32,
                    reasoning_tokens: u["completion_tokens_details"]["reasoning_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32,
                });
            }
        }
    }

    // A stream that ended without saying why was cut: by a stop, by a hold,
    // or by an upstream that hung up. Whatever it reported is for a partial
    // answer, and on the OpenAI protocol it reported nothing, because that
    // protocol says what a call cost only on its last chunk -- the one a cut
    // never delivers. The tokens were still generated and will still be
    // billed by the provider, so rather than record nothing, which reads
    // later as a free turn, the host estimates from what it sent and what
    // arrived and says that it did. Anthropic and Gemini report as they go,
    // so a cut stream on those routes keeps the real partial numbers.
    served.partial = finish_reason.is_none() && refused.is_none();
    if served.partial && usage.is_none() && received_chars > 0 {
        usage = Some(Usage {
            prompt_tokens: estimated_tokens(sent_chars),
            completion_tokens: estimated_tokens(received_chars),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        });
        served.usage_estimated = true;
    }

    // Ends the round as a failure rather than a short reply: what streamed
    // before it is still shown, but the turn does not go on as though the
    // model had finished.
    if let Some(mut refusal) = refused {
        refusal.model = served.model.clone();
        return Err(Refused(refusal).into());
    }

    for (index, signature) in call_signatures {
        if let Some(ContentPart::Call(call)) = call_slots.get(&index).and_then(|&i| parts.get(i)) {
            served.signed.calls.insert(call.id.clone(), signature);
        }
    }
    Ok((
        Completion {
            // A call with no name is a fragment of something that never arrived;
            // passing it on would have the guest dispatch on "".
            parts: parts
                .into_iter()
                .filter(|p| !matches!(p, ContentPart::Call(c) if c.name.is_empty()))
                // A call with no arguments streams none, and an empty string is
                // not JSON: kept as it came, every later round replayed it as
                // something no provider can read. Written as the empty object
                // it means, here where the call is assembled, so what is stored
                // and handed to the guest is already right.
                .map(|p| match p {
                    ContentPart::Call(mut c) if c.arguments.trim().is_empty() => {
                        c.arguments = "{}".to_string();
                        ContentPart::Call(c)
                    }
                    other => other,
                })
                .collect(),
            finish_reason,
            usage,
        },
        arrivals,
        served,
    ))
}

/// Who answered a call, as far as the gateway said.
pub(super) struct Served {
    /// Whether the gateway said somebody has asked this turn to stop. It
    /// reports it on the response it was already sending, the same way it
    /// reports a steer.
    pub(super) cancelled: bool,
    /// Why, where a hold stopped it rather than a person. The distinction
    /// matters afterwards: "you stopped this" and "the workspace was stopped"
    /// are different things to tell whoever reads the transcript.
    pub(super) held: Option<String>,
    pub(super) endpoint: Option<String>,
    pub(super) model: Option<String>,
    pub(super) paid_by: Option<String>,
    pub(super) provider_usage: Option<serde_json::Value>,
    pub(super) service_tier: Option<String>,
    /// The stream ended before the model said it had finished.
    pub(super) partial: bool,
    /// The usage is this host's guess, not the provider's report.
    pub(super) usage_estimated: bool,
    /// What the reply carries that only the model that wrote it can read:
    /// its signed thinking blocks, and the signatures on its calls.
    pub(super) signed: Signed,
}

/// A rough token count from a character count, for a call whose provider
/// never said. Four characters a token is the usual rule of thumb for
/// English prose and JSON alike; it is wrong for any particular text and
/// about right over a month of them, which is what a bill is. Rounded up, so
/// a single character is not a free one.
pub(super) fn estimated_tokens(chars: usize) -> u32 {
    chars.div_ceil(4) as u32
}

#[cfg(test)]
mod reasoning_tests {
    use super::thinking_in;

    #[test]
    fn thinking_is_read_from_either_name_providers_give_it() {
        for field in ["reasoning", "reasoning_content"] {
            let chunk = serde_json::json!({
                "choices": [{"delta": {"content": "", field: "the user wants"}}]
            });
            assert_eq!(
                thinking_in(&chunk),
                Some("the user wants"),
                "{field} should be recognized as thinking"
            );
        }
    }

    /// The shape that was losing whole turns: thinking arrives with `content`
    /// present and empty, so anything reading `content` alone sees nothing.
    #[test]
    fn a_thinking_chunk_carries_empty_content_beside_it() {
        let chunk = serde_json::json!({
            "choices": [{"delta": {"role": "assistant", "content": "", "reasoning": "The"}}]
        });
        assert_eq!(chunk["choices"][0]["delta"]["content"].as_str(), Some(""));
        assert_eq!(thinking_in(&chunk), Some("The"));
    }

    #[test]
    fn prose_is_not_thinking_and_an_empty_thought_is_not_one_either() {
        let prose = serde_json::json!({"choices": [{"delta": {"content": "Hello"}}]});
        assert_eq!(thinking_in(&prose), None);

        // Sent at the boundary between thinking and prose. Relaying it would
        // open a reasoning part with nothing in it.
        let closing = serde_json::json!({"choices": [{"delta": {"reasoning": ""}}]});
        assert_eq!(thinking_in(&closing), None);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A gateway that answers with `body` in two writes cut at `cut`, with a
    /// pause between so the client sees them as separate reads.
    async fn serve_cut(body: Vec<u8>, cut: usize) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 8192];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            socket.write_all(&body[..cut]).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            socket.write_all(&body[cut..]).await.unwrap();
        });
        url
    }

    #[tokio::test]
    async fn a_character_split_across_reads_does_not_fail_the_round() {
        let line = "{\"choices\":[{\"delta\":{\"content\":\"caf\u{e9} \u{1F4DA} done\"}}]}\n";
        let body = line.as_bytes().to_vec();
        let emoji = line.find('\u{1F4DA}').unwrap();
        // Inside the four-byte character, and inside the two-byte one.
        for cut in [emoji + 2, line.find('\u{e9}').unwrap() + 1] {
            let url = serve_cut(body.clone(), cut).await;
            let (completion, _, _) = stream_completion(
                &reqwest::Client::new(),
                &url,
                "token",
                "test",
                &uuid::Uuid::now_v7(),
                serde_json::json!({}),
                None,
                None,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
            assert!(
                completion
                    .parts
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t == "caf\u{e9} \u{1F4DA} done")),
                "cut at {cut}"
            );
        }
    }
}
