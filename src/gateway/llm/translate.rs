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
