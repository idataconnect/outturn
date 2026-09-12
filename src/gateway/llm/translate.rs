use super::types::*;

/// Convert OpenAI-format request to Anthropic Messages API format.
pub fn openai_to_anthropic(
    req: &ChatCompletionRequest,
) -> Result<serde_json::Value, TranslateError> {
    let mut system = None;
    let mut messages = Vec::new();

    for msg in &req.messages {
        match msg.role {
            Role::System => {
                let text = match &msg.content {
                    MessageContent::Text(t) => t.clone(),
                    MessageContent::Parts(parts) => parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                system = Some(text);
            }
            Role::User | Role::Assistant => {
                let content = match &msg.content {
                    MessageContent::Text(t) => serde_json::json!(t),
                    MessageContent::Parts(parts) => {
                        let blocks: Vec<serde_json::Value> = parts
                            .iter()
                            .map(|p| match p {
                                ContentPart::Text { text } => {
                                    serde_json::json!({"type": "text", "text": text})
                                }
                                ContentPart::ImageUrl { image_url } => {
                                    serde_json::json!({
                                        "type": "image",
                                        "source": {"type": "url", "url": image_url.url}
                                    })
                                }
                            })
                            .collect();
                        serde_json::json!(blocks)
                    }
                };

                let role = match msg.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    _ => unreachable!(),
                };

                // Handle tool calls in assistant messages
                if let Some(tool_calls) = &msg.tool_calls {
                    let mut blocks: Vec<serde_json::Value> = match &msg.content {
                        MessageContent::Text(t) if !t.is_empty() => {
                            vec![serde_json::json!({"type": "text", "text": t})]
                        }
                        _ => vec![],
                    };
                    for tc in tool_calls {
                        let input: serde_json::Value =
                            serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                        blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function.name,
                            "input": input,
                        }));
                    }
                    messages.push(serde_json::json!({"role": role, "content": blocks}));
                } else {
                    messages.push(serde_json::json!({"role": role, "content": content}));
                }
            }
            Role::Tool => {
                let tool_call_id = msg
                    .tool_call_id
                    .as_ref()
                    .ok_or(TranslateError("tool message missing tool_call_id".into()))?;
                let text = match &msg.content {
                    MessageContent::Text(t) => t.clone(),
                    MessageContent::Parts(_) => {
                        return Err(TranslateError(
                            "tool message with parts not supported".into(),
                        ));
                    }
                };
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": text,
                    }]
                }));
            }
        }
    }

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": messages,
    });

    if let Some(sys) = system {
        body["system"] = serde_json::json!(sys);
    }
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = serde_json::json!(max);
    } else {
        body["max_tokens"] = serde_json::json!(4096);
    }

    if let Some(tools) = &req.tools {
        let anthropic_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "input_schema": t.function.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::json!(anthropic_tools);
    }

    Ok(body)
}

/// Convert Anthropic Messages API response to OpenAI format.
pub fn anthropic_to_openai(
    resp: &serde_json::Value,
) -> Result<ChatCompletionResponse, TranslateError> {
    let id = resp["id"].as_str().unwrap_or("").to_string();
    let model = resp["model"].as_str().unwrap_or("").to_string();

    // Walked in order and kept in order. Anthropic's content is a sequence,
    // and a reply that says something, looks something up, then says something
    // more is three blocks whose arrangement is the whole of its meaning.
    let mut parts: Vec<Part> = Vec::new();

    if let Some(content) = resp["content"].as_array() {
        for block in content {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        parts.push(Part::Text { text: t.to_string() });
                    }
                }
                Some("tool_use") => {
                    parts.push(Part::ToolCall {
                        call: ToolCall {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            tool_type: "function".to_string(),
                            function: FunctionCall {
                                name: block["name"].as_str().unwrap_or("").to_string(),
                                arguments: block["input"].to_string(),
                            },
                        },
                    });
                }
                // Dropped: a thinking block cannot be replayed to the provider
                // on a later turn, so keeping it here would only put it in a
                // transcript that must not send it back. What it cost is still
                // counted -- see `reasoning_tokens` in the usage below.
                Some("thinking") | Some("redacted_thinking") => {}
                _ => {}
            }
        }
    }

    let (content_text, tool_calls) = Part::flatten(&parts);
    let message = Message {
        role: Role::Assistant,
        content: MessageContent::Text(content_text),
        name: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
    };

    let finish_reason = match resp["stop_reason"].as_str() {
        Some("end_turn") => Some("stop".to_string()),
        Some("tool_use") => Some("tool_calls".to_string()),
        Some("max_tokens") => Some("length".to_string()),
        other => other.map(String::from),
    };

    // Anthropic reports cache reads and cache writes separately from input,
    // and charges all three differently -- a write costs more than a fresh
    // token, a read costs a fraction. `input_tokens` already excludes both,
    // unlike the OpenAI protocol where cached tokens are folded into the
    // prompt total, so nothing is subtracted here.
    let usage = resp.get("usage").map(|u| {
        let input = u["input_tokens"].as_u64().unwrap_or(0) as u32;
        let output = u["output_tokens"].as_u64().unwrap_or(0) as u32;
        let cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32;
        let cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0) as u32;
        Usage {
            prompt_tokens: input,
            completion_tokens: output,
            total_tokens: input + output + cache_read + cache_write,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: cache_read,
                cache_creation_tokens: cache_write,
                extra: Default::default(),
            }),
            // Carried in the shape the OpenAI protocol uses, since that is the
            // canonical form everything downstream reads.
            completion_tokens_details: None,
            // And the original beside it, because the translation above is
            // lossy by design: Anthropic prices cache writes by TTL and the
            // canonical form has one number for them.
            extra: [("anthropic".to_string(), u.clone())].into_iter().collect(),
        }
    });

    Ok(ChatCompletionResponse {
        id,
        object: "chat.completion".to_string(),
        created: 0,
        model,
        choices: vec![Choice {
            index: 0,
            message,
            finish_reason,
            parts,
        }],
        usage,
    })
}

#[derive(Debug)]
pub struct TranslateError(pub String);

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    /// Anthropic's content is a sequence, and the sequence is the meaning.
    ///
    /// A reply that says something, looks something up, says something more,
    /// then looks something else up is four blocks whose arrangement cannot be
    /// recovered from "all the text" plus "all the calls". This is the case the
    /// old translation lost.
    #[test]
    fn interleaved_blocks_keep_their_order() {
        let resp = serde_json::json!({
            "id": "msg_1",
            "model": "claude-sonnet-5",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "Let me check that."},
                {"type": "tool_use", "id": "a", "name": "fetch", "input": {"url": "one"}},
                {"type": "text", "text": "And the other one."},
                {"type": "tool_use", "id": "b", "name": "fetch", "input": {"url": "two"}},
            ]
        });

        let out = anthropic_to_openai(&resp).expect("translate");
        let parts = &out.choices[0].parts;

        let shape: Vec<&str> = parts
            .iter()
            .map(|p| match p {
                Part::Text { .. } => "text",
                Part::ToolCall { .. } => "call",
            })
            .collect();
        assert_eq!(shape, ["text", "call", "text", "call"], "order lost: {parts:?}");

        match (&parts[0], &parts[1]) {
            (Part::Text { text }, Part::ToolCall { call }) => {
                assert_eq!(text, "Let me check that.");
                assert_eq!(call.id, "a");
            }
            other => panic!("wrong parts at the front: {other:?}"),
        }
    }

    /// The flattening still produces what the OpenAI-shaped fields expect, so
    /// nothing downstream changes until it is ready to read the parts.
    #[test]
    fn the_flattening_still_matches_the_openai_shape() {
        let resp = serde_json::json!({
            "id": "msg_2",
            "model": "claude-sonnet-5",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "one "},
                {"type": "tool_use", "id": "a", "name": "fetch", "input": {}},
                {"type": "text", "text": "two"},
            ]
        });

        let out = anthropic_to_openai(&resp).expect("translate");
        let msg = &out.choices[0].message;
        match &msg.content {
            MessageContent::Text(t) => assert_eq!(t, "one two"),
            other => panic!("expected flattened text, got {other:?}"),
        }
        assert_eq!(msg.tool_calls.as_ref().map(Vec::len), Some(1));
        // And the parts still hold what the flattening cannot say.
        assert_eq!(out.choices[0].parts.len(), 3);
    }

    /// A thinking block is not replayable, so it is dropped -- but it must not
    /// disturb the order of what remains.
    #[test]
    fn dropping_a_thinking_block_leaves_the_rest_in_order() {
        let resp = serde_json::json!({
            "id": "msg_3",
            "model": "claude-sonnet-5",
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "before"},
                {"type": "tool_use", "id": "a", "name": "fetch", "input": {}},
                {"type": "text", "text": "after"},
            ]
        });

        let parts = anthropic_to_openai(&resp).expect("translate").choices[0].parts.clone();
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[0], Part::Text { text: "before".into() });
        assert_eq!(parts[2], Part::Text { text: "after".into() });
    }
}

// Streaming --------------------------------------------------------------

/// Turns Anthropic's stream of typed events into OpenAI-shaped chunks.
///
/// The two protocols disagree about what a stream *is*. OpenAI sends one kind
/// of thing repeatedly, each a fragment of the same shape as the whole;
/// Anthropic sends a sequence of differently-typed events, and the meaning of
/// one depends on which block is open when it arrives. So this is a state
/// machine rather than a function per chunk: an `input_json_delta` is tool
/// arguments or a web search's innards depending only on what opened the block
/// it belongs to, and nothing in the event itself says which.
///
/// Usage is accumulated rather than replaced. Anthropic reports the input side
/// once, in `message_start`, and the output side as it grows, in
/// `message_delta`; a reader that keeps only the latest usage object it saw
/// would hold the output count and have thrown the input count away. That is
/// also what makes this protocol the forgiving one to interrupt -- whatever
/// arrived before the cut is real, and is kept.
#[derive(Debug, Default)]
pub struct AnthropicStream {
    id: String,
    model: String,
    /// What kind of content block is open, by index. An `input_json_delta`
    /// means tool arguments only when its block was opened as a `tool_use`.
    blocks: std::collections::HashMap<u32, BlockKind>,
    /// Tool calls are numbered in the order their blocks opened, which is not
    /// the same as Anthropic's block index: a reply that says something before
    /// calling a tool opens block 0 for text and block 1 for the call, and the
    /// call is still the first tool call.
    tool_slots: std::collections::HashMap<u32, u32>,
    next_tool_slot: u32,
    /// What has been reported so far, kept across events.
    usage: PartialUsage,
    /// Set once `message_delta` names one, which is the only place it appears.
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    ToolUse,
    /// Thinking, a server-side tool's result, or anything else this does not
    /// forward. Named rather than absent so a delta arriving for it is
    /// deliberately ignored instead of accidentally treated as text.
    Other,
}

/// Usage as it accumulates, which is not the same shape as usage once known.
///
/// Every field is optional because Anthropic reveals them at different times,
/// and "not yet said" has to be distinguishable from "said, and it was zero".
#[derive(Debug, Default, Clone)]
pub struct PartialUsage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read: Option<u32>,
    pub cache_write: Option<u32>,
    /// The provider's own usage objects, merged as they arrive. Kept because
    /// the translation below is lossy by design and a dimension this type does
    /// not model is still there to price later.
    pub raw: serde_json::Map<String, serde_json::Value>,
}

impl PartialUsage {
    /// Takes in what an event reported, keeping what it did not mention.
    ///
    /// `message_start` carries an `output_tokens` of 1 as a placeholder rather
    /// than a count. It is truthy, so a reader that accepts any value it is
    /// given records one token for every stream that stops before
    /// `message_delta` arrives. Anthropic's own opening value is therefore
    /// only taken when nothing better has been seen, and is overwritten by the
    /// first real count.
    fn absorb(&mut self, usage: &serde_json::Value, opening: bool) {
        let Some(object) = usage.as_object() else {
            return;
        };
        for (k, v) in object {
            self.raw.insert(k.clone(), v.clone());
        }
        let read = |name: &str| usage[name].as_u64().map(|n| n as u32);

        if let Some(n) = read("input_tokens") {
            self.input_tokens = Some(n);
        }
        if let Some(n) = read("cache_read_input_tokens") {
            self.cache_read = Some(n);
        }
        if let Some(n) = read("cache_creation_input_tokens") {
            self.cache_write = Some(n);
        }
        if let Some(n) = read("output_tokens") {
            // The opening event's count is a placeholder; a later one is real.
            if !opening || self.output_tokens.is_none() {
                self.output_tokens = Some(n);
            }
        }
    }

    /// Whether anything at all has been learned, which decides whether a chunk
    /// carries usage or omits it.
    pub fn known(&self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some()
    }

    /// The canonical form, for a chunk.
    ///
    /// Anthropic reports cache reads and writes *beside* the input count
    /// rather than inside it, the opposite of the OpenAI protocol, so nothing
    /// is subtracted here and the total adds all four.
    pub fn to_usage(&self) -> Usage {
        let input = self.input_tokens.unwrap_or(0);
        let output = self.output_tokens.unwrap_or(0);
        let cache_read = self.cache_read.unwrap_or(0);
        let cache_write = self.cache_write.unwrap_or(0);
        Usage {
            prompt_tokens: input,
            completion_tokens: output,
            total_tokens: input + output + cache_read + cache_write,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: cache_read,
                cache_creation_tokens: cache_write,
                extra: Default::default(),
            }),
            completion_tokens_details: None,
            extra: [(
                "anthropic".to_string(),
                serde_json::Value::Object(self.raw.clone()),
            )]
            .into_iter()
            .collect(),
        }
    }
}

impl AnthropicStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been learned about this call's cost so far.
    ///
    /// Asked for when a stream ends early, which is the whole reason usage is
    /// accumulated rather than read off the last event.
    pub fn usage(&self) -> &PartialUsage {
        &self.usage
    }

    /// Feeds one event in, and gets back the chunk it becomes, if any.
    ///
    /// Many events translate to nothing: `ping`, `content_block_stop`, the
    /// opening of a text block. They return `None` rather than an empty chunk,
    /// because a chunk with nothing in it still costs a reader a parse and
    /// still reaches a browser.
    pub fn event(&mut self, event: &serde_json::Value) -> Option<StreamChunk> {
        match event["type"].as_str()? {
            "message_start" => {
                let message = &event["message"];
                self.id = message["id"].as_str().unwrap_or_default().to_string();
                self.model = message["model"].as_str().unwrap_or_default().to_string();
                self.usage.absorb(&message["usage"], true);
                // The opening carries the input count, which is worth
                // forwarding at once: a turn stopped before its first token
                // still cost its prompt.
                Some(self.chunk(Delta::default(), None))
            }

            "content_block_start" => {
                let index = event["index"].as_u64().unwrap_or(0) as u32;
                let block = &event["content_block"];
                match block["type"].as_str() {
                    Some("text") => {
                        self.blocks.insert(index, BlockKind::Text);
                        None
                    }
                    Some("tool_use") => {
                        self.blocks.insert(index, BlockKind::ToolUse);
                        let slot = self.next_tool_slot;
                        self.next_tool_slot += 1;
                        self.tool_slots.insert(index, slot);
                        Some(self.chunk(
                            Delta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![ToolCallDelta {
                                    index: slot,
                                    id: Some(block["id"].as_str().unwrap_or_default().to_string()),
                                    tool_type: Some("function".to_string()),
                                    function: Some(FunctionCallDelta {
                                        name: Some(
                                            block["name"].as_str().unwrap_or_default().to_string(),
                                        ),
                                        arguments: Some(String::new()),
                                    }),
                                }]),
                            },
                            None,
                        ))
                    }
                    // Thinking and server-side tool blocks: noted so their
                    // deltas are ignored knowingly rather than mistaken for
                    // something to forward.
                    _ => {
                        self.blocks.insert(index, BlockKind::Other);
                        None
                    }
                }
            }

            "content_block_delta" => {
                let index = event["index"].as_u64().unwrap_or(0) as u32;
                let delta = &event["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str()?;
                        Some(self.chunk(
                            Delta {
                                role: None,
                                content: Some(text.to_string()),
                                tool_calls: None,
                            },
                            None,
                        ))
                    }
                    Some("input_json_delta") => {
                        // Only when the open block is a tool call. A web
                        // search's result carries this same delta type, and
                        // forwarding it would invent a tool call nothing asked
                        // for and nothing can answer.
                        if self.blocks.get(&index) != Some(&BlockKind::ToolUse) {
                            return None;
                        }
                        let slot = *self.tool_slots.get(&index)?;
                        let fragment = delta["partial_json"].as_str()?;
                        Some(self.chunk(
                            Delta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![ToolCallDelta {
                                    index: slot,
                                    id: None,
                                    tool_type: None,
                                    function: Some(FunctionCallDelta {
                                        name: None,
                                        // Passed through as it came. Arguments
                                        // are assembled by whoever consumes
                                        // them, so a stream cut mid-fragment
                                        // leaves the truncation visible rather
                                        // than hidden behind a repair.
                                        arguments: Some(fragment.to_string()),
                                    }),
                                }]),
                            },
                            None,
                        ))
                    }
                    _ => None,
                }
            }

            "message_delta" => {
                // Usage at the event's top level; the stop reason underneath
                // `delta`. The split is Anthropic's, and a reader that looks
                // for either in the other's place finds nothing.
                self.usage.absorb(&event["usage"], false);
                let reason = event["delta"]["stop_reason"]
                    .as_str()
                    .map(finish_reason_of)
                    .unwrap_or("stop");
                self.finish_reason = Some(reason.to_string());
                Some(self.chunk(Delta::default(), Some(reason.to_string())))
            }

            // Carries nothing the OpenAI shape has anywhere to put: the reason
            // and the usage both arrived in `message_delta` before it.
            "message_stop" | "ping" | "content_block_stop" => None,

            _ => None,
        }
    }

    /// Builds a chunk in the envelope every chunk carries.
    fn chunk(&self, delta: Delta, finish_reason: Option<String>) -> StreamChunk {
        StreamChunk {
            id: self.id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: self.model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta,
                finish_reason,
            }],
            usage: self.usage.known().then(|| self.usage.to_usage()),
            service_tier: None,
        }
    }
}

/// Anthropic's stop reasons, in the OpenAI protocol's vocabulary.
///
/// An unknown reason becomes "stop" rather than an error: a provider adding
/// one should not fail a turn that otherwise succeeded.
fn finish_reason_of(anthropic: &str) -> &'static str {
    match anthropic {
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn event(json: serde_json::Value) -> serde_json::Value {
        json
    }

    fn start(input: u32, output: u32) -> serde_json::Value {
        event(serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "model": "claude-test",
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
            }
        }))
    }

    /// The opening event reports `output_tokens: 1` as a placeholder, not a
    /// count. It is truthy, so a reader that takes any value it is handed
    /// bills a token for every stream that stops before the real count
    /// arrives -- which is every stream anyone presses stop on.
    #[test]
    fn the_opening_output_count_is_a_placeholder_and_is_replaced() {
        let mut s = AnthropicStream::new();
        s.event(&start(100, 1));
        assert_eq!(s.usage().output_tokens, Some(1), "taken, for want of anything better");

        s.event(&event(serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 42 },
        })));
        assert_eq!(
            s.usage().output_tokens,
            Some(42),
            "the real count must replace the placeholder, not be ignored \
             because something was already there"
        );
    }

    /// The input side arrives once and the output side arrives later. Keeping
    /// only the most recent usage object would hold the second and lose the
    /// first, which is a turn that cost its prompt for free.
    #[test]
    fn usage_accumulates_rather_than_replacing() {
        let mut s = AnthropicStream::new();
        s.event(&start(1000, 1));
        s.event(&event(serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            // As Anthropic sends it: the output side only.
            "usage": { "output_tokens": 25 },
        })));

        let usage = s.usage().to_usage();
        assert_eq!(usage.prompt_tokens, 1000, "the input count survived the later event");
        assert_eq!(usage.completion_tokens, 25);
    }

    /// A stream cut after some text still knows the prompt it was answering,
    /// which is the whole reason this protocol is the forgiving one to stop.
    #[test]
    fn a_stream_cut_partway_still_reports_what_it_knows() {
        let mut s = AnthropicStream::new();
        s.event(&start(500, 1));
        s.event(&event(serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" }
        })));
        s.event(&event(serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "half a sent" }
        })));
        // and then nothing: no message_delta, no message_stop.

        assert!(s.usage().known(), "a cut stream knows something and should say so");
        assert_eq!(s.usage().to_usage().prompt_tokens, 500);
    }

    /// Cache tokens sit beside the input count rather than inside it, so the
    /// prompt is the sum. Subtracting -- which is right for Gemini and for
    /// the OpenAI protocol -- would undercount every cached conversation.
    #[test]
    fn cache_tokens_are_added_to_the_input_count_not_taken_out_of_it() {
        let mut s = AnthropicStream::new();
        s.event(&event(serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "m", "model": "claude-test",
                "usage": {
                    "input_tokens": 80,
                    "output_tokens": 1,
                    "cache_read_input_tokens": 20,
                    "cache_creation_input_tokens": 5,
                },
            }
        })));

        let usage = s.usage().to_usage();
        assert_eq!(usage.prompt_tokens, 80, "input_tokens already excludes the cache");
        let details = usage.prompt_tokens_details.expect("cache detail");
        assert_eq!(details.cached_tokens, 20);
        assert_eq!(details.cache_creation_tokens, 5);
        assert_eq!(usage.total_tokens, 80 + 1 + 20 + 5, "all four make the total");
    }

    /// Tool calls are numbered in the order they open, which is not the block
    /// index: a reply that speaks before calling opens block 0 for its text.
    #[test]
    fn a_tool_call_after_text_is_still_the_first_tool_call() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        s.event(&event(serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "text", "text": "" }
        })));
        let chunk = s
            .event(&event(serde_json::json!({
                "type": "content_block_start", "index": 1,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": "search" }
            })))
            .expect("a tool call opens a chunk");

        let call = &chunk.choices[0].delta.tool_calls.as_ref().expect("tool calls")[0];
        assert_eq!(call.index, 0, "the first tool call, though it is the second block");
        assert_eq!(call.id.as_deref(), Some("toolu_1"));
        assert_eq!(
            call.function.as_ref().and_then(|f| f.name.as_deref()),
            Some("search")
        );
    }

    /// `input_json_delta` means tool arguments only inside a tool block. A
    /// server-side search reports its innards with the same delta type, and
    /// forwarding those would invent a call nothing can answer.
    #[test]
    fn json_deltas_outside_a_tool_block_are_not_tool_arguments() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        s.event(&event(serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "web_search_tool_result", "id": "srv_1" }
        })));

        let chunk = s.event(&event(serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{\"q\":" }
        })));
        assert!(
            chunk.is_none(),
            "a delta from a block that is not a tool call became a tool call"
        );
    }

    /// Arguments pass through as fragments. A stream cut mid-fragment leaves
    /// invalid JSON, which is the truth about what arrived and is better said
    /// than repaired into something the model never asked for.
    #[test]
    fn tool_arguments_pass_through_as_fragments() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        s.event(&event(serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "f" }
        })));

        let mut assembled = String::new();
        for fragment in [r#"{"city""#, r#":"San Fra"#] {
            let chunk = s
                .event(&event(serde_json::json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": { "type": "input_json_delta", "partial_json": fragment }
                })))
                .expect("a fragment becomes a chunk");
            let call = &chunk.choices[0].delta.tool_calls.as_ref().expect("calls")[0];
            assert_eq!(call.index, 0, "fragments correlate by index");
            assert!(call.id.is_none(), "only the opening chunk names the call");
            assembled.push_str(
                call.function.as_ref().and_then(|f| f.arguments.as_deref()).unwrap_or_default(),
            );
        }

        assert_eq!(assembled, r#"{"city":"San Fra"#);
        assert!(
            serde_json::from_str::<serde_json::Value>(&assembled).is_err(),
            "a cut stream leaves arguments that do not parse, and that is the point"
        );
    }

    #[test]
    fn stop_reasons_become_the_other_protocols_vocabulary() {
        assert_eq!(finish_reason_of("end_turn"), "stop");
        assert_eq!(finish_reason_of("tool_use"), "tool_calls");
        assert_eq!(finish_reason_of("max_tokens"), "length");
        assert_eq!(finish_reason_of("refusal"), "content_filter");
        assert_eq!(finish_reason_of("something_new"), "stop", "an unknown reason is not an error");
    }

    /// Events that carry nothing the OpenAI shape can hold produce nothing.
    #[test]
    fn events_with_nothing_to_say_produce_no_chunk() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        for kind in ["ping", "message_stop", "content_block_stop"] {
            assert!(
                s.event(&event(serde_json::json!({ "type": kind, "index": 0 }))).is_none(),
                "{kind} should not become an empty chunk a reader has to parse"
            );
        }
    }
}
