use super::types::*;
use outturn_core::facts::ModelFacts;

/// An image URL as the Messages API takes it.
///
/// The chat-completions protocol carries an inline image as a `data:` URL,
/// which is how the runtime sends what an agent asks about; see
/// `outturn_core::vision`. Anthropic has no URL type for those -- a data URL
/// sent as a `url` source is refused as not fetchable -- so it is unpacked
/// into the base64 source it already is. Anything else is a URL the provider
/// fetches itself.
fn anthropic_image(url: &str) -> serde_json::Value {
    match data_url(url) {
        Some((media_type, data)) => serde_json::json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }),
        None => serde_json::json!({
            "type": "image",
            "source": {"type": "url", "url": url}
        }),
    }
}

/// The media type and base64 payload of a `data:` URL, where that is what
/// `url` is. Only the base64 form is recognized: it is the one both this
/// protocol's image part and every image a model is shown use.
fn data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (header, data) = rest.split_once(',')?;
    let media_type = header.strip_suffix(";base64")?;
    Some((media_type, data))
}

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
                // Joined rather than the last one winning: outturn sends one,
                // at the top, and a second arriving here is a mistake upstream
                // -- which should cost nothing worse than a longer prompt, not
                // the system prompt silently replaced.
                system = Some(match system {
                    Some(existing) => format!("{existing}\n{text}"),
                    None => text,
                });
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
                                    anthropic_image(&image_url.url)
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
                    // Thinking first, as the reply had it: the model decided,
                    // then spoke, then called. Only a reply that called a tool
                    // carries them: the runtime keeps them for the rounds of
                    // one turn, and nothing yet keeps them past it.
                    let mut blocks: Vec<serde_json::Value> = msg.thinking_blocks.clone();
                    if let MessageContent::Text(t) = &msg.content
                        && !t.is_empty()
                    {
                        blocks.push(serde_json::json!({"type": "text", "text": t}));
                    }
                    for tc in tool_calls {
                        // Anthropic takes only an object here. The runtime now
                        // stores an empty call as `{}`, but transcripts written
                        // before it hold an empty string, and anything that is
                        // not an object would fail every later round of the
                        // conversation, not one call -- so it goes as an empty
                        // one.
                        let input = match serde_json::from_str(&tc.function.arguments) {
                            Ok(object @ serde_json::Value::Object(_)) => object,
                            _ => serde_json::json!({}),
                        };
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

    // Two cache breakpoints, the most that pays without knowing more about the
    // conversation than its shape. Anthropic caches only what is marked.
    //
    // The system prompt's: tools render before it, so one marker covers both,
    // and both change only when somebody edits the agent -- every session of
    // that agent reads it.
    //
    // The last message's: it moves forward every round, tool results
    // included, so each request reads the one before it. Marking only
    // completed turns was the earlier design, and a long tool loop would
    // carry its breakpoint past the twenty blocks Anthropic looks back to
    // find the previous entry, rewriting the whole conversation unseen.
    // An empty one is left out: Anthropic refuses an empty text block, and
    // there is nothing in it to cache.
    if let Some(sys) = system.filter(|sys| !sys.is_empty()) {
        body["system"] = serde_json::json!([{
            "type": "text",
            "text": sys,
            "cache_control": {"type": "ephemeral"},
        }]);
    }
    if let Some(last) = body["messages"].as_array_mut().and_then(|m| m.last_mut()) {
        mark_for_cache(last);
    }
    let facts = req.facts.as_deref().cloned().unwrap_or_default();
    // A model that refuses sampling parameters refuses the whole request over
    // one, so it is left out -- except 1, the one value these models accept.
    if let Some(temp) = req.temperature {
        if facts.sampling || temp == 1.0 {
            body["temperature"] = serde_json::json!(temp);
        } else {
            tracing::debug!(model = %req.model, temp, "temperature left out: this model refuses it");
        }
    }
    // Required by the protocol, so filled in when the request names none. At
    // the model's own limit: thinking counts against it, the model is not told
    // where it is, and a small one cuts a hard answer off mid-thought. Effort
    // is what controls how much is written; this is only the guarantee.
    body["max_tokens"] = serde_json::json!(
        req.max_tokens
            .or(facts.output_limit)
            .unwrap_or(UNDESCRIBED_OUTPUT_LIMIT)
    );
    if facts.adaptive_thinking {
        apply_thinking(&mut body, &facts, req.reasoning_effort.as_deref());
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

/// `max_tokens` for a model nobody described, where the protocol insists on
/// one: small enough that no Claude refuses it. A model that writes more is
/// given its limit by describing it (docs/models.md).
const UNDESCRIBED_OUTPUT_LIMIT: u32 = 4096;

/// Asks an adaptive model for its reasoning, and sets how much of it.
///
/// Summarized, because the default returns thinking blocks with nothing in
/// them, and the person reading the transcript should see why a turn did what
/// it did -- the reasoning is billed the same whether it is shown or not.
///
/// `none` leaves `thinking` out rather than sending `disabled`, which the
/// always-on models refuse, and asks for the least effort the model offers:
/// on those, leaving it out does not stop it thinking. A level the model does
/// not offer becomes the highest one it does.
/// The beta that lets a request say what to do with a stale thinking block.
pub const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

fn apply_thinking(body: &mut serde_json::Value, facts: &ModelFacts, effort: Option<&str>) {
    let effort = match effort {
        Some("none") => {
            if let Some(lowest) = facts.efforts.first() {
                body["output_config"] = serde_json::json!({"effort": lowest});
            }
            return;
        }
        Some("minimal") => Some("low"),
        other => other,
    };
    // A thinking block given back is checked against everything before it,
    // and refused with a 400 -- for every later turn of the session -- if
    // any of it changed: a system prompt composed afresh, history trimmed or
    // compacted. Dropping the stale block instead costs that block's
    // reasoning and nothing else (docs/thinking.md).
    body["thinking"] = serde_json::json!({"type": "adaptive", "display": "summarized"});
    if facts.thinking_binding {
        body["thinking"]["block_binding"] =
            serde_json::json!({"prefix_mismatch_behavior": "drop_block"});
        body["anthropic_beta"] = serde_json::json!([THINKING_BINDING_BETA]);
    }
    let Some(asked) = effort else {
        return;
    };
    let level = if facts.efforts.iter().any(|e| e == asked) {
        Some(asked)
    } else if ["xhigh", "max"].contains(&asked) {
        facts.efforts.last().map(String::as_str)
    } else {
        None
    };
    match level {
        Some(level) => body["output_config"] = serde_json::json!({"effort": level}),
        None => tracing::warn!(
            effort = asked,
            "an effort this model does not offer; left to its default"
        ),
    }
}

/// Mark a message's last block as the end of a cached prefix.
///
/// A message whose content is a bare string has no block to carry the marker,
/// so it becomes one text block -- the same prompt to the model. An empty
/// text block is left unmarked, since Anthropic refuses a marker on one.
fn mark_for_cache(message: &mut serde_json::Value) {
    if let Some(text) = message["content"].as_str() {
        if text.is_empty() {
            return;
        }
        message["content"] = serde_json::json!([{"type": "text", "text": text}]);
    }
    let Some(block) = message["content"]
        .as_array_mut()
        .and_then(|blocks| blocks.last_mut())
    else {
        return;
    };
    if block["type"] == "text" && block["text"].as_str().is_none_or(str::is_empty) {
        return;
    }
    block["cache_control"] = serde_json::json!({"type": "ephemeral"});
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
                        parts.push(Part::Text {
                            text: t.to_string(),
                        });
                    }
                }
                Some("tool_use") => {
                    parts.push(Part::ToolCall {
                        call: ToolCall {
                            provider_signature: None,
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
        thinking_blocks: Vec::new(),
        signed_by: None,
    };

    // The same mapping the stream applies, so a refusal reads as one on
    // either path. This path used to pass `refusal` and anything newer
    // through in Anthropic's own words, and dropped the explanation.
    let stop = stop_of(resp["stop_reason"].as_str(), &resp["stop_details"]);

    // The same conversion the stream applies, from the same counts.
    let usage = resp.get("usage").map(|u| {
        let mut counts = PartialUsage::default();
        counts.absorb(u, false);
        counts.to_usage()
    });

    Ok(ChatCompletionResponse {
        id,
        object: "chat.completion".to_string(),
        created: 0,
        model,
        choices: vec![Choice {
            index: 0,
            message,
            finish_reason: Some(stop.finish_reason),
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
mod image_tests {
    use super::*;

    fn asked_with(url: &str) -> ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image_url", "image_url": {"url": url}},
            ]}],
        }))
        .expect("request")
    }

    /// The runtime sends an image inline as a data URL. The Messages API has
    /// no URL source for those -- it refuses one as not fetchable -- so it
    /// goes as the base64 source it already is, with the media type the data
    /// URL names.
    #[test]
    fn an_inline_image_reaches_anthropic_as_base64() {
        let body = openai_to_anthropic(&asked_with("data:image/png;base64,iVBORw0KGgo="))
            .expect("translate");
        let image = &body["messages"][0]["content"][1];
        assert_eq!(image["type"], "image");
        assert_eq!(
            image["source"],
            serde_json::json!({
                "type": "base64",
                "media_type": "image/png",
                "data": "iVBORw0KGgo=",
            })
        );
        assert_eq!(body["messages"][0]["content"][0]["text"], "what is this?");
    }

    /// A URL the provider can fetch still goes as one.
    #[test]
    fn a_fetchable_image_url_reaches_anthropic_as_a_url() {
        let body =
            openai_to_anthropic(&asked_with("https://example.com/cat.png")).expect("translate");
        assert_eq!(
            body["messages"][0]["content"][1]["source"],
            serde_json::json!({"type": "url", "url": "https://example.com/cat.png"})
        );
    }

    /// Gemini is shown the image, not just the question about it. Text-only
    /// translation sent a model a question about a picture it never got.
    #[test]
    fn an_inline_image_reaches_gemini_as_inline_data() {
        let body =
            openai_to_gemini(&asked_with("data:image/jpeg;base64,/9j/4AAQ")).expect("translate");
        let parts = body["contents"][0]["parts"].as_array().expect("parts");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(
            parts[1]["inlineData"],
            serde_json::json!({"mimeType": "image/jpeg", "data": "/9j/4AAQ"})
        );
    }

    /// A URL goes to Gemini as a file reference rather than being dropped.
    #[test]
    fn a_fetchable_image_url_reaches_gemini_as_file_data() {
        let body = openai_to_gemini(&asked_with("https://example.com/cat.png")).expect("translate");
        assert_eq!(
            body["contents"][0]["parts"][1]["fileData"]["fileUri"],
            "https://example.com/cat.png"
        );
    }

    /// Only the base64 form of a data URL is one here; anything else is left
    /// for the provider to make sense of.
    #[test]
    fn a_data_url_is_split_into_type_and_payload() {
        assert_eq!(
            data_url("data:image/webp;base64,UklGR"),
            Some(("image/webp", "UklGR"))
        );
        assert_eq!(data_url("data:text/plain,hello"), None);
        assert_eq!(data_url("https://example.com/a.png"), None);
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn translated(messages: serde_json::Value) -> serde_json::Value {
        let request: ChatCompletionRequest =
            serde_json::from_value(serde_json::json!({"model": "m", "messages": messages}))
                .expect("request");
        openai_to_anthropic(&request).expect("translate")
    }

    /// Signed thinking goes back first in the reply that called the tool,
    /// as it came, and is never sent on to an OpenAI-compatible endpoint.
    #[test]
    fn thinking_goes_back_before_the_call_it_led_to() {
        let thinking = serde_json::json!({"type": "thinking", "thinking": "t", "signature": "s"});
        let body = translated(serde_json::json!([
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": "Checking.", "thinking_blocks": [thinking],
             "tool_calls": [{"id": "c1", "type": "function",
                             "function": {"name": "weather", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "fog"},
        ]));
        let reply = &body["messages"][1]["content"];
        assert_eq!(reply[0], thinking);
        assert_eq!(reply[1]["type"], "text");
        assert_eq!(reply[2]["type"], "tool_use");

        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{"role": "assistant", "content": "", "thinking_blocks": [thinking]}],
        }))
        .expect("request");
        assert!(
            !serde_json::to_string(&request)
                .unwrap()
                .contains("thinking_blocks"),
            "a passthrough body carries only fields the protocol defines"
        );
    }

    fn marked(block: &serde_json::Value) -> bool {
        block["cache_control"] == serde_json::json!({"type": "ephemeral"})
    }

    /// The system prompt and the newest message each end a cached prefix, and
    /// nothing in between does: two breakpoints of Anthropic's four.
    #[test]
    fn the_system_prompt_and_the_last_message_are_marked() {
        let body = translated(serde_json::json!([
            {"role": "system", "content": "Be useful."},
            {"role": "user", "content": "First question"},
            {"role": "assistant", "content": "First answer"},
            {"role": "user", "content": "Second question"},
        ]));

        assert_eq!(body["system"][0]["text"], "Be useful.");
        assert!(marked(&body["system"][0]), "system unmarked: {body}");

        let messages = body["messages"].as_array().expect("messages");
        let last = &messages[messages.len() - 1]["content"][0];
        assert_eq!(last["text"], "Second question");
        assert!(marked(last), "last message unmarked: {body}");

        let earlier = serde_json::to_string(&messages[..messages.len() - 1]).expect("json");
        assert!(
            !earlier.contains("cache_control"),
            "marked too early: {earlier}"
        );
    }

    /// Mid-loop the newest message is a tool result, and it is the one marked,
    /// so each round reads the round before it.
    #[test]
    fn a_tool_result_carries_the_moving_breakpoint() {
        let body = translated(serde_json::json!([
            {"role": "user", "content": "What's the weather?"},
            {"role": "assistant", "content": "", "tool_calls": [{
                "id": "c1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{}"},
            }]},
            {"role": "tool", "tool_call_id": "c1", "content": "Foggy"},
        ]));

        let last = &body["messages"][2]["content"][0];
        assert_eq!(last["type"], "tool_result");
        assert!(marked(last), "tool result unmarked: {body}");
    }

    /// A call with no arguments arrives as an empty string, and is replayed as
    /// an empty object: Anthropic refuses null, and refused it every round.
    #[test]
    fn a_call_without_arguments_is_replayed_as_an_empty_object() {
        let body = translated(serde_json::json!([
            {"role": "user", "content": "Ping it"},
            {"role": "assistant", "content": "", "tool_calls": [{
                "id": "c1", "type": "function",
                "function": {"name": "ping", "arguments": ""},
            }]},
            {"role": "tool", "tool_call_id": "c1", "content": "pong"},
        ]));
        assert_eq!(
            body["messages"][1]["content"][0]["input"],
            serde_json::json!({})
        );
    }

    fn asked(model: &str, effort: Option<&str>, temperature: Option<f32>) -> serde_json::Value {
        let mut request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .expect("request");
        request.reasoning_effort = effort.map(str::to_string);
        request.temperature = temperature;
        let held = outturn_core::catalog::shipped();
        let entry = outturn_core::catalog::entry(&held, "anthropic", model);
        request.facts = entry.map(|e| std::sync::Arc::new(ModelFacts::of(Some(e), None)));
        openai_to_anthropic(&request).expect("translate")
    }

    /// The current models are asked for their reasoning, summarized, with
    /// room to write it: their own output limit rather than 4096, which
    /// thinking counted against and a hard answer ran out of.
    #[test]
    fn a_current_model_is_asked_to_show_its_thinking_with_room_to_answer() {
        let body = asked("claude-sonnet-5-5", Some("medium"), None);
        assert_eq!(
            body["thinking"],
            serde_json::json!({
                "type": "adaptive",
                "display": "summarized",
                "block_binding": {"prefix_mismatch_behavior": "drop_block"},
            })
        );
        assert_eq!(
            body["anthropic_beta"],
            serde_json::json!([THINKING_BINDING_BETA])
        );
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["max_tokens"], 128_000);
    }

    /// `none` cannot turn an always-on model's thinking off, and `disabled` is
    /// refused outright, so it asks for the least there is instead.
    #[test]
    fn none_asks_for_the_least_effort_rather_than_disabling() {
        let body = asked("claude-opus-5-5", Some("none"), None);
        assert!(body.get("thinking").is_none(), "{body}");
        assert_eq!(body["output_config"]["effort"], "low");
    }

    /// A level the model does not offer becomes its highest, and `minimal`
    /// is `low`.
    #[test]
    fn an_effort_is_clamped_to_what_the_model_offers() {
        assert_eq!(
            asked("claude-opus-4-6", Some("xhigh"), None)["output_config"]["effort"],
            "max"
        );
        assert_eq!(
            asked("claude-sonnet-5-5", Some("minimal"), None)["output_config"]["effort"],
            "low"
        );
    }

    /// The current models refuse a temperature outright -- the request, not
    /// the parameter -- except 1. The 4.6 line still takes one.
    #[test]
    fn a_temperature_reaches_only_a_model_that_takes_it() {
        assert!(
            asked("claude-sonnet-5-5", None, Some(0.7))
                .get("temperature")
                .is_none()
        );
        assert_eq!(
            asked("claude-sonnet-5-5", None, Some(1.0))["temperature"],
            1.0
        );
        let older = asked("claude-sonnet-4-6", None, Some(0.5));
        assert_eq!(older["temperature"], 0.5);
    }

    /// The 4.x line thinks the same way as the 5.x but is not sent its
    /// stale-block setting, which some platforms refuse from it; an older
    /// model is not asked about thinking at all.
    #[test]
    fn thinking_is_asked_for_as_the_model_takes_it() {
        assert!(
            asked("claude-opus-4-6", Some("high"), None)["thinking"]
                .get("block_binding")
                .is_none()
        );
        assert!(
            asked("claude-haiku-4-5", Some("high"), None)
                .get("thinking")
                .is_none()
        );
    }

    /// A model nobody described is sent what it was asked and a
    /// `max_tokens` no Claude refuses, whatever its name suggests.
    #[test]
    fn a_model_nobody_described_is_not_guessed_at() {
        let body = asked("claude-something-new", Some("high"), Some(0.5));
        assert!(body.get("thinking").is_none(), "{body}");
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_tokens"], UNDESCRIBED_OUTPUT_LIMIT);
    }

    /// An empty system prompt is no system prompt: as a block it would be an
    /// empty one, which Anthropic refuses on every round.
    #[test]
    fn an_empty_system_prompt_is_left_out() {
        let body = translated(serde_json::json!([
            {"role": "system", "content": ""},
            {"role": "user", "content": "hi"},
        ]));
        assert!(body.get("system").is_none(), "{body}");
    }

    /// Anthropic refuses a marker on an empty text block, so one is left bare.
    #[test]
    fn an_empty_last_message_is_not_marked() {
        let body = translated(serde_json::json!([
            {"role": "user", "content": ""},
        ]));
        assert!(!body.to_string().contains("cache_control"), "{body}");
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
        assert_eq!(
            shape,
            ["text", "call", "text", "call"],
            "order lost: {parts:?}"
        );

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

        let parts = anthropic_to_openai(&resp).expect("translate").choices[0]
            .parts
            .clone();
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(
            parts[0],
            Part::Text {
                text: "before".into()
            }
        );
        assert_eq!(
            parts[2],
            Part::Text {
                text: "after".into()
            }
        );
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
    /// Thinking blocks being assembled, by index, until they close: the text
    /// streams in many deltas and the signature after it in one more.
    thinking: std::collections::HashMap<u32, (String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    ToolUse,
    /// The model's reasoning, summarized: forwarded as `reasoning`, where the
    /// runtime already reads other providers' thinking.
    Thinking,
    /// A server-side tool's result, or anything else this does not
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

    /// The canonical form, which the whole-response path builds too: the
    /// one conversion of Anthropic's counts, so the two paths cannot drift.
    ///
    /// Anthropic reports cache reads and cache writes *beside* the input
    /// count rather than inside it, and charges all three differently -- a
    /// write costs more than a fresh token, a read costs a fraction. Its
    /// `input_tokens` is therefore already the full-rate count the canonical
    /// `prompt_tokens` means (see the definition on `Usage`), so nothing is
    /// subtracted here and the total adds all four.
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
            // Carried in the shape the OpenAI protocol uses, since that is the
            // canonical form everything downstream reads.
            completion_tokens_details: None,
            // And the original beside it, because the translation above is
            // lossy by design: Anthropic prices cache writes by TTL and the
            // canonical form has one number for them.
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
                    Some("thinking") => {
                        self.blocks.insert(index, BlockKind::Thinking);
                        self.thinking.insert(index, Default::default());
                        None
                    }
                    // Nothing to show, but it is the model's and goes back
                    // with the reply, whole, like any other thinking block.
                    Some("redacted_thinking") => {
                        self.blocks.insert(index, BlockKind::Other);
                        Some(self.chunk(
                            Delta {
                                thinking_block: Some(block.clone()),
                                ..Delta::default()
                            },
                            None,
                        ))
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
                                    provider_signature: None,
                                    id: Some(block["id"].as_str().unwrap_or_default().to_string()),
                                    tool_type: Some("function".to_string()),
                                    function: Some(FunctionCallDelta {
                                        name: Some(
                                            block["name"].as_str().unwrap_or_default().to_string(),
                                        ),
                                        arguments: Some(String::new()),
                                    }),
                                }]),
                                reasoning: None,
                                reasoning_content: None,
                                thinking_block: None,
                                refusal: None,
                                refusal_category: None,
                            },
                            None,
                        ))
                    }
                    // Server-side tool blocks and redacted thinking: noted so their
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
                                reasoning: None,
                                reasoning_content: None,
                                thinking_block: None,
                                refusal: None,
                                refusal_category: None,
                            },
                            None,
                        ))
                    }
                    Some("thinking_delta") => {
                        if self.blocks.get(&index) != Some(&BlockKind::Thinking) {
                            return None;
                        }
                        let thinking = delta["thinking"].as_str()?;
                        if let Some((text, _)) = self.thinking.get_mut(&index) {
                            text.push_str(thinking);
                        }
                        Some(self.chunk(
                            Delta {
                                role: None,
                                content: None,
                                tool_calls: None,
                                reasoning: Some(thinking.to_string()),
                                reasoning_content: None,
                                thinking_block: None,
                                refusal: None,
                                refusal_category: None,
                            },
                            None,
                        ))
                    }
                    Some("signature_delta") => {
                        if let Some((_, signature)) = self.thinking.get_mut(&index) {
                            signature.push_str(delta["signature"].as_str().unwrap_or_default());
                        }
                        None
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
                                    provider_signature: None,
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
                                reasoning: None,
                                reasoning_content: None,
                                thinking_block: None,
                                refusal: None,
                                refusal_category: None,
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
                let stop = stop_of(
                    event["delta"]["stop_reason"].as_str(),
                    &event["delta"]["stop_details"],
                );
                self.finish_reason = Some(stop.finish_reason.clone());
                let (refusal, refusal_category) = match stop.refusal {
                    Some((reason, category)) => (Some(reason), category),
                    None => (None, None),
                };
                Some(self.chunk(
                    Delta {
                        refusal,
                        refusal_category,
                        ..Delta::default()
                    },
                    Some(stop.finish_reason),
                ))
            }

            // Carries nothing the OpenAI shape has anywhere to put: the reason
            // and the usage both arrived in `message_delta` before it.
            "message_stop" | "ping" => None,

            // A thinking block is complete only here, with its signature
            // after its text. One left unsigned -- a stream cut before the
            // signature came -- is let go: the provider refuses it back.
            "content_block_stop" => {
                let index = event["index"].as_u64().unwrap_or(0) as u32;
                let (text, signature) = self.thinking.remove(&index)?;
                if signature.is_empty() {
                    return None;
                }
                Some(self.chunk(
                    Delta {
                        thinking_block: Some(serde_json::json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": signature,
                        })),
                        ..Delta::default()
                    },
                    None,
                ))
            }

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

/// How a reply ended, read once for both paths.
struct Stop {
    /// In the OpenAI protocol's vocabulary.
    finish_reason: String,
    /// What a person is told and which category the provider filed it under,
    /// where the provider declined.
    refusal: Option<(String, Option<String>)>,
}

/// Anthropic's stop reason and details, in the OpenAI protocol's terms.
///
/// The whole response carries them at its top level and the stream carries
/// them under `message_delta`'s `delta`; the mapping from there is the same,
/// and was once written twice, disagreeing on what a refusal became.
fn stop_of(stop_reason: Option<&str>, details: &serde_json::Value) -> Stop {
    let refused = stop_reason == Some("refusal");
    Stop {
        finish_reason: stop_reason
            .map(finish_reason_of)
            .unwrap_or("stop")
            .to_string(),
        refusal: refused.then(|| {
            (
                refusal_of(details),
                details["category"].as_str().map(str::to_string),
            )
        }),
    }
}

/// What a person is told when the provider declined: the explanation's first
/// sentence, which says what was objected to. The rest is addressed to
/// whoever integrated the API, so it is logged rather than shown, with the
/// category the provider filed it under.
fn refusal_of(details: &serde_json::Value) -> String {
    let explanation = details["explanation"].as_str().unwrap_or_default();
    tracing::warn!(
        category = details["category"].as_str().unwrap_or("unspecified"),
        explanation,
        "the provider declined to answer"
    );
    match explanation.split_inclusive(". ").next().map(str::trim) {
        Some(first) if !first.is_empty() => first.to_string(),
        _ => "The model declined to answer this request.".to_string(),
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
mod response_tests {
    use super::*;

    /// The same facts every protocol's test states: 80 fresh, 20 read from
    /// cache, 5 written. Anthropic says 80 and lists the rest beside it, and
    /// the canonical numbers are the same ones the stream produces.
    #[test]
    fn a_whole_response_counts_its_cache_the_way_the_stream_does() {
        let resp = anthropic_to_openai(&serde_json::json!({
            "id": "m", "model": "claude",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 80, "output_tokens": 7,
                "cache_read_input_tokens": 20, "cache_creation_input_tokens": 5,
            },
        }))
        .expect("translate");
        let usage = resp.usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 80, "full-rate input only");
        let details = usage.prompt_tokens_details.expect("cache detail");
        assert_eq!(details.cached_tokens, 20);
        assert_eq!(details.cache_creation_tokens, 5);
        assert_eq!(usage.total_tokens, 112);
        assert_eq!(
            usage.extra["anthropic"]["cache_read_input_tokens"], 20,
            "the provider's own object rides through"
        );
    }

    /// A refusal is a content filter on this path as on the stream, and an
    /// unknown reason is an ordinary stop; neither used to be.
    #[test]
    fn stop_reasons_match_the_streams() {
        let finish = |reason: &str| {
            anthropic_to_openai(&serde_json::json!({
                "content": [], "stop_reason": reason,
                "stop_details": {"explanation": "No. Really not.", "category": "x"},
            }))
            .expect("translate")
            .choices[0]
                .finish_reason
                .clone()
        };
        assert_eq!(finish("refusal").as_deref(), Some("content_filter"));
        assert_eq!(finish("tool_use").as_deref(), Some("tool_calls"));
        assert_eq!(finish("something_new").as_deref(), Some("stop"));
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

    /// Summarized thinking arrives as `reasoning`, where the runtime reads
    /// every provider's. The signature says nothing to a reader, so it waits
    /// for the block to close and goes on with the whole block.
    #[test]
    fn thinking_streams_as_reasoning_and_not_as_text() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        assert!(
            s.event(&event(serde_json::json!({
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "thinking", "thinking": "", "signature": ""},
            })))
            .is_none()
        );
        let chunk = s
            .event(&event(serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "I need to count"},
            })))
            .expect("a reasoning chunk");
        let delta = &chunk.choices[0].delta;
        assert_eq!(delta.reasoning.as_deref(), Some("I need to count"));
        assert_eq!(delta.content, None, "thinking is not the reply");
        assert!(
            s.event(&event(serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "signature_delta", "signature": "c2ln"},
            })))
            .is_none()
        );
        let closed = s
            .event(&event(
                serde_json::json!({"type": "content_block_stop", "index": 0}),
            ))
            .expect("the block, once it closes");
        assert_eq!(
            closed.choices[0].delta.thinking_block,
            Some(serde_json::json!({
                "type": "thinking", "thinking": "I need to count", "signature": "c2ln",
            }))
        );
        assert_eq!(closed.choices[0].delta.reasoning, None, "not shown twice");
    }

    /// A refusal says why, in a sentence a person can read, rather than
    /// ending the stream as though the model had finished.
    #[test]
    fn a_refusal_says_why() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        let chunk = s
            .event(&event(serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": "refusal", "stop_details": {
                    "type": "refusal", "category": "reasoning_extraction",
                    "explanation": "This request was blocked as it seems to violate the terms. API integrators: configure a fallback.",
                }},
                "usage": {"output_tokens": 0},
            })))
            .expect("the closing chunk");
        assert_eq!(
            chunk.choices[0].finish_reason.as_deref(),
            Some("content_filter")
        );
        assert_eq!(
            chunk.choices[0].delta.refusal.as_deref(),
            Some("This request was blocked as it seems to violate the terms.")
        );
    }

    /// A stream cut before the signature leaves a block the provider will
    /// not take back, so none is offered.
    #[test]
    fn an_unsigned_thinking_block_is_not_kept() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        s.event(&event(serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "thinking", "thinking": "", "signature": ""},
        })));
        s.event(&event(serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "half a"},
        })));
        assert!(
            s.event(&event(
                serde_json::json!({"type": "content_block_stop", "index": 0}),
            ))
            .is_none()
        );
    }

    /// Redacted thinking has nothing to show and is kept whole all the same.
    #[test]
    fn redacted_thinking_is_kept_as_it_came() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        let block = serde_json::json!({"type": "redacted_thinking", "data": "b3BhcXVl"});
        let chunk = s
            .event(&event(serde_json::json!({
                "type": "content_block_start", "index": 0, "content_block": block,
            })))
            .expect("the block");
        assert_eq!(chunk.choices[0].delta.thinking_block, Some(block));
        assert!(
            s.event(&event(
                serde_json::json!({"type": "content_block_stop", "index": 0}),
            ))
            .is_none()
        );
    }

    /// The opening event reports `output_tokens: 1` as a placeholder, not a
    /// count. It is truthy, so a reader that takes any value it is handed
    /// bills a token for every stream that stops before the real count
    /// arrives -- which is every stream anyone presses stop on.
    #[test]
    fn the_opening_output_count_is_a_placeholder_and_is_replaced() {
        let mut s = AnthropicStream::new();
        s.event(&start(100, 1));
        assert_eq!(
            s.usage().output_tokens,
            Some(1),
            "taken, for want of anything better"
        );

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
        assert_eq!(
            usage.prompt_tokens, 1000,
            "the input count survived the later event"
        );
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

        assert!(
            s.usage().known(),
            "a cut stream knows something and should say so"
        );
        assert_eq!(s.usage().to_usage().prompt_tokens, 500);
    }

    /// Cache tokens sit beside the input count rather than inside it, so the
    /// prompt is the sum. Subtracting -- which is right for Gemini and for
    /// the OpenAI protocol -- would undercount every cached conversation.
    ///
    /// The same facts every protocol's test states: 80 fresh, 20 read from
    /// cache, 5 written.
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
        assert_eq!(
            usage.prompt_tokens, 80,
            "input_tokens already excludes the cache"
        );
        let details = usage.prompt_tokens_details.expect("cache detail");
        assert_eq!(details.cached_tokens, 20);
        assert_eq!(details.cache_creation_tokens, 5);
        assert_eq!(
            usage.total_tokens,
            80 + 1 + 20 + 5,
            "all four make the total"
        );
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

        let call = &chunk.choices[0]
            .delta
            .tool_calls
            .as_ref()
            .expect("tool calls")[0];
        assert_eq!(
            call.index, 0,
            "the first tool call, though it is the second block"
        );
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
                call.function
                    .as_ref()
                    .and_then(|f| f.arguments.as_deref())
                    .unwrap_or_default(),
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
        assert_eq!(
            finish_reason_of("something_new"),
            "stop",
            "an unknown reason is not an error"
        );
    }

    /// Events that carry nothing the OpenAI shape can hold produce nothing.
    #[test]
    fn events_with_nothing_to_say_produce_no_chunk() {
        let mut s = AnthropicStream::new();
        s.event(&start(10, 1));
        for kind in ["ping", "message_stop", "content_block_stop"] {
            assert!(
                s.event(&event(serde_json::json!({ "type": kind, "index": 0 })))
                    .is_none(),
                "{kind} should not become an empty chunk a reader has to parse"
            );
        }
    }
}

// Gemini -----------------------------------------------------------------

/// How much of a flattened tool exchange is put in front of a model.
///
/// The same budget the agent's own truncator works to, so a result small
/// enough to show once stays small enough when replayed in a different shape.
pub const MAX_FLATTENED_BYTES: usize = 50 * 1024;

/// Renders a tool call and its result as prose.
///
/// Used when a stored call cannot be replayed as a call: a signature belongs
/// to the model that issued it, so a conversation that changed models has tool
/// calls a newer Gemini will refuse and an older one never signed.
///
/// The alternatives are worse. Dropping the signature is refused outright.
/// Google publishes a sentinel that turns the refusal off, but it asks the
/// provider to skip a check on data known to be invalid and costs reasoning
/// quality in a way nothing reports. A rendering keeps every byte of what
/// happened and moves it somewhere no validation applies, and what the model
/// receives is exactly what can be read here.
///
/// Head and tail rather than head alone, on the same reasoning as the agent's
/// truncator: a result's beginning says what it is and its end is often the
/// answer, while the middle of an over-long one rarely decides anything.
pub fn flatten_tool_exchange(name: &str, arguments: &str, result: &str) -> String {
    let head = if arguments.is_empty() {
        format!("[tool] {name}")
    } else {
        format!("[tool] {name}({arguments})")
    };
    let budget = MAX_FLATTENED_BYTES.saturating_sub(head.len() + 64);

    if result.len() <= budget {
        return format!("{head} -> {result}");
    }

    // Cut on character boundaries, or slicing panics on a multi-byte character
    // that happens to straddle the cut.
    let mut cut = (budget * 7 / 10).min(result.len());
    while cut > 0 && !result.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut tail_at = result.len().saturating_sub(budget.saturating_sub(cut));
    while tail_at < result.len() && !result.is_char_boundary(tail_at) {
        tail_at += 1;
    }

    format!(
        "{head} -> {}\n[... {} of {} bytes elided ...]\n{}",
        &result[..cut],
        tail_at - cut,
        result.len(),
        &result[tail_at..],
    )
}

/// A tool's parameters, in the subset of JSON Schema Gemini accepts.
///
/// Gemini validates a function declaration against its own schema type rather
/// than against JSON Schema, and refuses the whole request -- not the field --
/// when it meets a keyword it does not model. `additionalProperties` is the
/// one a tool hits first, declaring a map of strings the way every other
/// provider expects; the request comes back 400 naming a path several levels
/// deep, and the turn fails over to whatever is configured next.
///
/// Stripped rather than translated, because these say what a *caller* may
/// send and the caller here is the model: a schema that forgets to forbid
/// extra properties is looser than intended, where a schema Gemini refuses is
/// a tool the model cannot use at all.
fn gemini_schema(schema: &serde_json::Value) -> serde_json::Value {
    // Keywords Gemini's `Schema` has no field for. Composition keywords are
    // included deliberately: a `oneOf` it cannot represent would otherwise
    // take the whole declaration down with it.
    const UNSUPPORTED: &[&str] = &[
        "additionalProperties",
        "$schema",
        "$id",
        "$ref",
        "$defs",
        "definitions",
        "allOf",
        "oneOf",
        "not",
        "patternProperties",
        "const",
        "examples",
        "default",
    ];

    match schema {
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .filter(|(k, _)| !UNSUPPORTED.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), gemini_schema(v)))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(gemini_schema).collect())
        }
        other => other.clone(),
    }
}

/// The major version of a Gemini model, where its name states one.
///
/// Parsed rather than matched on a substring: "gemini-3" appearing anywhere in
/// a name is not the same question as the model being version 3 or newer, and
/// a tuned model called `my-gemini-30-thing` would answer the substring test
/// wrongly in both directions.
pub fn gemini_major_version(model: &str) -> Option<u32> {
    let rest = model.strip_prefix("gemini-")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Whether this model wants call ids on its function calls and responses.
///
/// Gemini 3 and newer match a response to its call by id and return one of
/// their own. Older models refuse the field outright, so it cannot simply be
/// sent to everybody and left to be ignored.
pub fn gemini_wants_call_ids(model: &str) -> bool {
    gemini_major_version(model).is_some_and(|v| v >= 3)
}

/// Convert an OpenAI-format request into Gemini's `generateContent` shape.
///
/// Gemini differs in three ways that matter: the system prompt is its own
/// field rather than a message, the assistant is called "model", and a tool
/// result is a message part rather than a message role.
pub fn openai_to_gemini(req: &ChatCompletionRequest) -> Result<serde_json::Value, TranslateError> {
    openai_to_gemini_for(req, &req.model)
}

/// An image URL as Gemini takes it: inline bytes for a `data:` URL, which is
/// how the runtime sends what an agent asks about, and a file reference for
/// anything else.
fn gemini_image(url: &str) -> serde_json::Value {
    match data_url(url) {
        Some((media_type, data)) => serde_json::json!({
            "inlineData": { "mimeType": media_type, "data": data }
        }),
        None => serde_json::json!({ "fileData": { "fileUri": url } }),
    }
}

/// As above, for a named target model.
///
/// The model decides two things that cannot be got right without knowing it:
/// whether call ids are wanted, and whether a stored signature may be
/// replayed. A signature is a token one model issued about its own reasoning,
/// so replaying it to a different one is at best meaningless.
pub fn openai_to_gemini_for(
    req: &ChatCompletionRequest,
    target_model: &str,
) -> Result<serde_json::Value, TranslateError> {
    let wants_ids = gemini_wants_call_ids(target_model);
    let mut contents: Vec<serde_json::Value> = Vec::new();
    let mut system: Option<String> = None;
    // Calls rendered as prose rather than replayed. Their results are rendered
    // to match: a `functionResponse` answering a call the model was never
    // shown is an answer to nothing.
    let mut rendered_calls: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Each call's name, by id. A result in this protocol need not say which
    // function answered -- the id says which call -- but Gemini refuses a
    // `functionResponse` without a name, so it is looked up from the call.
    let mut call_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for call in req
        .messages
        .iter()
        .flat_map(|m| m.tool_calls.iter().flatten())
    {
        call_names.insert(call.id.clone(), call.function.name.clone());
    }

    for message in &req.messages {
        let text = match &message.content {
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

        match message.role {
            Role::System => {
                // Repeated system messages are joined rather than the last one
                // winning: dropping one silently changes what was asked.
                system = Some(match system {
                    Some(existing) => format!("{existing}\n{text}"),
                    None => text,
                });
            }
            Role::Tool => {
                let name = message
                    .name
                    .clone()
                    .or_else(|| {
                        message
                            .tool_call_id
                            .as_ref()
                            .and_then(|id| call_names.get(id).cloned())
                    })
                    .unwrap_or_default();
                // Its call was rendered, so this is rendered to match.
                if message
                    .tool_call_id
                    .as_ref()
                    .is_some_and(|id| rendered_calls.contains(id))
                {
                    contents.push(serde_json::json!({
                        "role": "user",
                        "parts": [{ "text": flatten_tool_exchange(
                            if name.is_empty() { "tool" } else { &name },
                            "",
                            &text,
                        ) }],
                    }));
                    continue;
                }

                let mut response = serde_json::json!({
                    "name": name,
                    "response": { "content": text },
                });
                // Newer models match a response to its call by id; older ones
                // refuse the field, so it is sent only where it is wanted.
                if wants_ids && let Some(id) = &message.tool_call_id {
                    response["id"] = serde_json::json!(id);
                }
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{ "functionResponse": response }],
                }));
            }
            Role::User => {
                // Each part as it came, rather than the text alone: the parts
                // form is how an image travels, and a user message reduced to
                // its text asks the model about a picture it was never shown.
                let parts: Vec<serde_json::Value> = match &message.content {
                    MessageContent::Text(t) => vec![serde_json::json!({ "text": t })],
                    MessageContent::Parts(parts) => parts
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text { text } => serde_json::json!({ "text": text }),
                            ContentPart::ImageUrl { image_url } => gemini_image(&image_url.url),
                        })
                        .collect(),
                };
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": parts,
                }));
            }
            Role::Assistant => {
                // A batch is signed on its first call only, so the question is
                // whether the turn carries a signature at all, not whether
                // every call does. A turn with none came from a model that is
                // no longer answering, and replaying its calls is refused.
                let calls: Vec<&ToolCall> = message.tool_calls.iter().flatten().collect();
                // Signed by another model is as good as unsigned: this one
                // refuses it, and the exchange is rendered instead.
                let signed_here = message
                    .signed_by
                    .as_deref()
                    .is_none_or(|by| by == target_model);
                if !calls.is_empty()
                    && (!signed_here || !calls.iter().any(|c| c.provider_signature.is_some()))
                {
                    let mut rendered = text.clone();
                    for call in &calls {
                        rendered_calls.insert(call.id.clone());
                        if !rendered.is_empty() {
                            rendered.push('\n');
                        }
                        rendered.push_str(&flatten_tool_exchange(
                            &call.function.name,
                            &call.function.arguments,
                            "(result follows)",
                        ));
                    }
                    contents.push(serde_json::json!({
                        "role": "model",
                        "parts": [{ "text": rendered }],
                    }));
                    continue;
                }

                let mut parts: Vec<serde_json::Value> = Vec::new();
                if !text.is_empty() {
                    parts.push(serde_json::json!({ "text": text }));
                }
                for call in message.tool_calls.iter().flatten() {
                    let mut function_call = serde_json::json!({
                        "name": call.function.name,
                        // Arguments travel as a string in the OpenAI shape
                        // and as an object here. A string that will not
                        // parse becomes an empty object rather than
                        // failing the turn: the model wrote it, and the
                        // turn is already answering for that.
                        "args": serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                            .unwrap_or_else(|_| serde_json::json!({})),
                    });
                    if wants_ids && !call.id.is_empty() {
                        function_call["id"] = serde_json::json!(call.id);
                    }

                    let mut part = serde_json::json!({ "functionCall": function_call });
                    // Back onto the call it came with, and onto no other. A
                    // parallel batch is signed once, on its first call; the
                    // siblings are unsigned and must stay so, because every
                    // replayed copy is billed as the previous turn's reasoning
                    // again and nothing says so at the time.
                    if let Some(signature) = &call.provider_signature {
                        part["thoughtSignature"] = serde_json::json!(signature);
                    }
                    parts.push(part);
                }
                contents.push(serde_json::json!({ "role": "model", "parts": parts }));
            }
        }
    }

    let mut body = serde_json::json!({ "contents": contents });
    if let Some(system) = system {
        body["systemInstruction"] = serde_json::json!({ "parts": [{ "text": system }] });
    }
    if let Some(tools) = &req.tools {
        let declarations: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "parameters": t.function.parameters.as_ref().map(gemini_schema),
                })
            })
            .collect();
        body["tools"] = serde_json::json!([{ "functionDeclarations": declarations }]);
    }
    let mut generation = serde_json::Map::new();
    if let Some(temperature) = req.temperature {
        generation.insert("temperature".into(), serde_json::json!(temperature));
    }
    if let Some(max) = req.max_tokens {
        generation.insert("maxOutputTokens".into(), serde_json::json!(max));
    }
    if let Some(level) = gemini_thinking_level(req) {
        generation.insert(
            "thinkingConfig".into(),
            serde_json::json!({ "thinkingLevel": level }),
        );
    }
    if !generation.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(generation);
    }

    Ok(body)
}

/// The thinking level to ask a Gemini model for, from the effort asked of it
/// and the levels the catalog says it offers.
///
/// Clamped as Anthropic's is: "none" and "minimal" are its lowest, since no
/// Gemini 3 model turns thinking off; "xhigh" and "max" its highest; and a
/// level it does not offer otherwise is left to its default rather than sent
/// to be refused. A model the catalog gives no levels -- 2.5, which takes a
/// token budget instead -- is left alone.
fn gemini_thinking_level(req: &ChatCompletionRequest) -> Option<String> {
    let asked = req.reasoning_effort.as_deref()?;
    let offered = &req.facts.as_ref()?.efforts;
    let level = match asked {
        "none" | "minimal" => offered.first(),
        "xhigh" | "max" => offered.last(),
        asked => offered.iter().find(|e| *e == asked),
    };
    if level.is_none() && !offered.is_empty() {
        tracing::warn!(
            effort = asked,
            "an effort this model does not offer; left to its default"
        );
    }
    level.cloned()
}

/// Gemini's usage object, in the canonical shape.
///
/// `cachedContentTokenCount` is *included* in `promptTokenCount`, where
/// Anthropic reports its cache tokens beside the input count. So it is taken
/// out here where Anthropic's is added, and getting the direction wrong
/// produces a plausible number rather than an obviously broken one.
///
/// `thoughtsTokenCount` is billed at output rates but is not part of
/// `candidatesTokenCount`, so it is carried separately rather than folded in.
pub fn gemini_usage(usage: &serde_json::Value) -> Option<Usage> {
    let object = usage.as_object()?;
    let read = |name: &str| usage[name].as_u64().unwrap_or(0) as u32;

    let prompt = read("promptTokenCount");
    let cached = read("cachedContentTokenCount");
    let candidates = read("candidatesTokenCount");
    let thoughts = read("thoughtsTokenCount");

    Some(Usage {
        // What was actually evaluated: the prompt less whatever the cache
        // served. Subtracted, because Gemini counts them inside the total.
        prompt_tokens: prompt.saturating_sub(cached),
        completion_tokens: candidates,
        total_tokens: read("totalTokenCount").max(prompt + candidates + thoughts),
        prompt_tokens_details: Some(PromptTokensDetails {
            cached_tokens: cached,
            // Gemini charges for creating a cache through a separate call, not
            // as part of a completion, so nothing here is a cache write.
            cache_creation_tokens: 0,
            extra: Default::default(),
        }),
        completion_tokens_details: (thoughts > 0).then(|| CompletionTokensDetails {
            reasoning_tokens: thoughts,
            extra: Default::default(),
        }),
        // The original beside the translation, because the mapping above
        // cannot carry every dimension and a bill may later need one it lost.
        extra: [(
            "gemini".to_string(),
            serde_json::Value::Object(object.clone()),
        )]
        .into_iter()
        .collect(),
    })
}

/// Gemini's finish reasons, in the OpenAI protocol's vocabulary.
pub fn gemini_finish_reason(reason: &str, called_a_tool: bool) -> String {
    match reason {
        // Gemini says STOP for a function call as much as for prose; the
        // distinction the other protocols draw has to be recovered from what
        // the reply actually contained.
        "STOP" if called_a_tool => "tool_calls".to_string(),
        "STOP" => "stop".to_string(),
        "MAX_TOKENS" => "length".to_string(),
        "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" => {
            "content_filter".to_string()
        }
        _ => "stop".to_string(),
    }
}

/// Turns Gemini's stream into OpenAI-shaped chunks.
///
/// Simpler than Anthropic's, because Gemini repeats whole candidates rather
/// than deltas of an open block: each chunk carries the parts produced since
/// the last one, and usage arrives as a running total on nearly every chunk.
/// That last property is why an interrupted Gemini turn still knows most of
/// what it cost -- there is no final event holding the only copy.
#[derive(Debug, Default)]
pub struct GeminiStream {
    id: String,
    model: String,
    /// Numbered in the order they appear, since Gemini does not index them.
    next_tool_slot: u32,
    /// The last usage seen, which is cumulative rather than incremental.
    usage: Option<Usage>,
    /// Whether any function call has been forwarded, which decides what a
    /// finish reason of STOP means.
    called_a_tool: bool,
    /// A signature that arrived on a text part rather than on a call.
    ///
    /// For prose, Gemini signs the block at its end, on a part carrying no
    /// text at all -- measured: seven chunks, the first six with words and no
    /// signature, the seventh with 3496 characters of signature and an empty
    /// string. A reader that forwards only parts with something in them
    /// discards it, and the next round is refused for want of it.
    text_signature: Option<String>,
    /// Which price tier served the call. Gemini names it in `usageMetadata`
    /// rather than beside the model, and it multiplies every token on the
    /// call, so it travels with the usage rather than being left behind in
    /// the provider's own object.
    service_tier: Option<String>,
}

impl GeminiStream {
    pub fn new(model: String) -> Self {
        Self {
            id: "gemini".to_string(),
            model,
            ..Default::default()
        }
    }

    /// What the call has cost so far, for a turn that ends early.
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }

    /// The signature covering this reply's text, if it was signed.
    ///
    /// Separate from the one on a tool call because they sign different parts
    /// and must go back on the parts they came from. Merging them would send
    /// one signature twice, which is billed twice.
    pub fn text_signature(&self) -> Option<&str> {
        self.text_signature.as_deref()
    }

    pub fn event(&mut self, event: &serde_json::Value) -> Option<StreamChunk> {
        // Taken before anything else: a chunk carrying only usage is still
        // worth reading, and on this protocol most of them carry it.
        if let Some(usage) = gemini_usage(&event["usageMetadata"]) {
            self.usage = Some(usage);
        }
        if let Some(tier) = event["usageMetadata"]["serviceTier"].as_str() {
            self.service_tier = Some(tier.to_string());
        }

        let candidate = &event["candidates"][0];
        let mut content: Option<String> = None;
        let mut tool_calls: Vec<ToolCallDelta> = Vec::new();

        for part in candidate["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(text) = part["text"].as_str() {
                content.get_or_insert_with(String::new).push_str(text);
                // Kept whenever it is offered and never unset by a later part
                // that has none: the signature is the block's, not the
                // chunk's, and only one chunk of the block carries it.
                if let Some(signature) = part["thoughtSignature"].as_str() {
                    self.text_signature = Some(signature.to_string());
                }
            } else if let Some(signature) = part["thoughtSignature"].as_str() {
                // A part with a signature and nothing else. It is the whole
                // reason this branch exists.
                self.text_signature = Some(signature.to_string());
            }
            if let Some(call) = part.get("functionCall").filter(|c| !c.is_null()) {
                let slot = self.next_tool_slot;
                self.next_tool_slot += 1;
                self.called_a_tool = true;
                tool_calls.push(ToolCallDelta {
                    index: slot,
                    // From the part this call arrived on, and never from any
                    // other: Gemini signs the first call of a parallel batch
                    // and bills the previous turn's reasoning again for every
                    // copy replayed, without saying so.
                    provider_signature: part["thoughtSignature"].as_str().map(str::to_string),
                    // Gemini 3 and newer name their own call ids, and a
                    // function response is matched back by id on those models.
                    // Kept rather than replaced: an invented id matches
                    // nothing. Older models send none, so one is made up for
                    // them, which is harmless where nothing compares it.
                    id: Some(
                        call["id"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("gemini_call_{slot}")),
                    ),
                    tool_type: Some("function".to_string()),
                    function: Some(FunctionCallDelta {
                        name: Some(call["name"].as_str().unwrap_or_default().to_string()),
                        // Whole, not in fragments: this protocol sends a
                        // finished object rather than partial JSON.
                        arguments: Some(call["args"].to_string()),
                    }),
                });
            }
        }

        let finish = candidate["finishReason"]
            .as_str()
            .map(|r| gemini_finish_reason(r, self.called_a_tool));
        // Said, not left as a stream that merely ended: the provider's own
        // message where it gave one, else which filter it was.
        let refusal = (finish.as_deref() == Some("content_filter")).then(|| {
            let reason = candidate["finishReason"].as_str().unwrap_or_default();
            tracing::warn!(reason, "the provider declined to answer");
            match candidate["finishMessage"].as_str() {
                Some(said) if !said.trim().is_empty() => said.trim().to_string(),
                _ => format!("The model's provider declined to answer this request ({reason})."),
            }
        });

        // Nothing to say and nothing to end: usage was the whole of it, and
        // that has been kept.
        if content.is_none() && tool_calls.is_empty() && finish.is_none() {
            return None;
        }

        Some(StreamChunk {
            id: self.id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: self.model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content,
                    tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                    reasoning: None,
                    reasoning_content: None,
                    thinking_block: None,
                    refusal_category: refusal.is_some().then(|| {
                        candidate["finishReason"]
                            .as_str()
                            .unwrap_or_default()
                            .to_lowercase()
                    }),
                    refusal,
                },
                finish_reason: finish,
            }],
            usage: self.usage.clone(),
            service_tier: self.service_tier.clone(),
        })
    }
}

/// Convert a whole Gemini response into the canonical shape.
pub fn gemini_to_openai(
    resp: &serde_json::Value,
) -> Result<ChatCompletionResponse, TranslateError> {
    let candidate = &resp["candidates"][0];

    let mut parts: Vec<Part> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for part in candidate["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(t) = part["text"].as_str() {
            parts.push(Part::Text {
                text: t.to_string(),
            });
        }
        if let Some(call) = part.get("functionCall").filter(|c| !c.is_null()) {
            let tool_call = ToolCall {
                // Gemini names its own call ids now. Kept rather than
                // replaced: a function response is matched back by id, and an
                // invented one matches nothing.
                id: call["id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("gemini_call_{}", tool_calls.len())),
                tool_type: "function".to_string(),
                function: FunctionCall {
                    name: call["name"].as_str().unwrap_or_default().to_string(),
                    arguments: call["args"].to_string(),
                },
                // Rides the same part as the call, and is required back.
                provider_signature: part["thoughtSignature"].as_str().map(str::to_string),
            };
            parts.push(Part::ToolCall {
                call: tool_call.clone(),
            });
            tool_calls.push(tool_call);
        }
    }

    let finish_reason = candidate["finishReason"]
        .as_str()
        .map(|r| gemini_finish_reason(r, !tool_calls.is_empty()));

    // Joined as the stream joins them, with nothing between: several text
    // parts are one reply arriving in pieces, and this path once put a line
    // break between them that the streamed copy never had.
    let (text, _) = Part::flatten(&parts);

    Ok(ChatCompletionResponse {
        id: "gemini".to_string(),
        object: "chat.completion".to_string(),
        created: 0,
        model: resp["modelVersion"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: Role::Assistant,
                content: MessageContent::Text(text),
                name: None,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                tool_call_id: None,
                thinking_blocks: Vec::new(),
                signed_by: None,
            },
            finish_reason,
            parts,
        }],
        usage: gemini_usage(&resp["usageMetadata"]),
    })
}

#[cfg(test)]
mod gemini_tests {
    use super::*;

    /// Gemini counts cached tokens *inside* the prompt total, so recovering
    /// what was actually evaluated means subtracting. Anthropic reports them
    /// beside its input count, where the same arithmetic would be adding.
    /// Both directions are plausible; only one is right per provider.
    #[test]
    fn cached_tokens_come_out_of_the_prompt_total() {
        let usage = gemini_usage(&serde_json::json!({
            "promptTokenCount": 1000,
            "cachedContentTokenCount": 800,
            "candidatesTokenCount": 50,
            "totalTokenCount": 1050,
        }))
        .expect("usage");

        assert_eq!(
            usage.prompt_tokens, 200,
            "the cache served 800 of the 1000, so 200 were evaluated; adding \
             would have claimed 1800 and billed a cached conversation twice"
        );
        let details = usage.prompt_tokens_details.expect("cache detail");
        assert_eq!(details.cached_tokens, 800);
        assert_eq!(
            details.cache_creation_tokens, 0,
            "a completion never writes a Gemini cache; that is a separate call"
        );
    }

    /// Thinking tokens are billed at output rates but are not part of
    /// `candidatesTokenCount`, so folding them in would double-count and
    /// leaving them out entirely would lose them.
    #[test]
    fn thinking_tokens_are_carried_separately_from_the_answer() {
        let usage = gemini_usage(&serde_json::json!({
            "promptTokenCount": 100,
            "candidatesTokenCount": 20,
            "thoughtsTokenCount": 300,
            "totalTokenCount": 420,
        }))
        .expect("usage");

        assert_eq!(usage.completion_tokens, 20, "the answer is the answer");
        assert_eq!(
            usage
                .completion_tokens_details
                .expect("reasoning detail")
                .reasoning_tokens,
            300,
            "thinking is counted, and counted apart"
        );
        assert_eq!(usage.total_tokens, 420);
    }

    /// The provider's own object is kept whole beside the translation, since
    /// the mapping cannot carry every dimension and a disputed bill is argued
    /// from what arrived rather than from what was made of it.
    #[test]
    fn the_providers_own_usage_is_kept_verbatim() {
        let usage = gemini_usage(&serde_json::json!({
            "promptTokenCount": 10,
            "candidatesTokenCount": 5,
            "somethingNewGoogleAdded": 7,
        }))
        .expect("usage");

        let raw = &usage.extra["gemini"];
        assert_eq!(
            raw["somethingNewGoogleAdded"], 7,
            "a dimension we do not model survived"
        );
    }

    /// Gemini says STOP for a function call as much as for prose, so what the
    /// reply contained is the only way to tell the two apart.
    #[test]
    fn a_function_call_finishing_is_not_an_ordinary_stop() {
        assert_eq!(gemini_finish_reason("STOP", false), "stop");
        assert_eq!(gemini_finish_reason("STOP", true), "tool_calls");
        assert_eq!(gemini_finish_reason("MAX_TOKENS", false), "length");
        assert_eq!(gemini_finish_reason("SAFETY", false), "content_filter");
        assert_eq!(gemini_finish_reason("SOMETHING_NEW", false), "stop");
    }

    /// A running total on every chunk is what leaves an interrupted Gemini
    /// turn still able to say what it cost.
    #[test]
    fn a_stream_cut_partway_keeps_the_last_total_it_saw() {
        let mut s = GeminiStream::new("gemini-test".into());
        s.event(&serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "one " }] }, "index": 0 }],
            "usageMetadata": { "promptTokenCount": 100, "candidatesTokenCount": 1, "totalTokenCount": 101 },
        }));
        s.event(&serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "two" }] }, "index": 0 }],
            "usageMetadata": { "promptTokenCount": 100, "candidatesTokenCount": 2, "totalTokenCount": 102 },
        }));
        // and then the stream is cut: no finishReason, no final event.

        let usage = s.usage().expect("a cut stream still knows what it saw");
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(
            usage.completion_tokens, 2,
            "the running total as of the cut"
        );
    }

    /// Gemini sends finished argument objects rather than partial JSON, so a
    /// call arrives whole or not at all.
    #[test]
    fn a_function_call_arrives_with_its_arguments_whole() {
        let mut s = GeminiStream::new("gemini-test".into());
        let chunk = s
            .event(&serde_json::json!({
                "candidates": [{
                    "content": { "parts": [{
                        "functionCall": { "name": "lookup", "args": { "city": "San Francisco" } }
                    }] },
                    "finishReason": "STOP",
                    "index": 0,
                }],
            }))
            .expect("a function call becomes a chunk");

        let call = &chunk.choices[0].delta.tool_calls.as_ref().expect("calls")[0];
        assert_eq!(
            call.function.as_ref().unwrap().name.as_deref(),
            Some("lookup")
        );
        let arguments = call
            .function
            .as_ref()
            .unwrap()
            .arguments
            .as_deref()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(arguments).expect("valid json")["city"],
            "San Francisco"
        );
        assert_eq!(
            chunk.choices[0].finish_reason.as_deref(),
            Some("tool_calls"),
            "STOP with a call in it is a tool call, not a plain stop"
        );
    }

    /// Taken verbatim from a real `gemini-2.5-flash` stream, because a fixture
    /// written from the documentation agrees with the documentation and a
    /// fixture written from the wire agrees with the thing being billed.
    ///
    /// Two facts here were not obvious and are worth keeping honest: thinking
    /// tokens are real and are *not* inside `candidatesTokenCount` (16 + 50 +
    /// 28 = 94, the total Google reports), and a service tier arrives in the
    /// usage object rather than beside the model.
    /// Several text parts are one reply in pieces, joined on both paths
    /// with nothing between them.
    #[test]
    fn text_parts_are_joined_the_way_the_stream_joins_them() {
        let parts = serde_json::json!({"parts": [{"text": "one "}, {"text": "two"}]});
        let resp = gemini_to_openai(&serde_json::json!({
            "candidates": [{"content": parts, "finishReason": "STOP"}],
        }))
        .expect("translate");
        let MessageContent::Text(whole) = &resp.choices[0].message.content else {
            panic!("text");
        };
        let mut stream = GeminiStream::new("m".into());
        let chunk = stream
            .event(&serde_json::json!({"candidates": [{"content": parts}]}))
            .expect("chunk");
        assert_eq!(whole, "one two");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("one two"));
    }

    #[test]
    fn a_real_gemini_usage_object_adds_up() {
        let real = serde_json::json!({
            "promptTokenCount": 16,
            "candidatesTokenCount": 50,
            "totalTokenCount": 94,
            "promptTokensDetails": [{ "modality": "TEXT", "tokenCount": 16 }],
            "thoughtsTokenCount": 28,
            "serviceTier": "standard"
        });

        let usage = gemini_usage(&real).expect("usage");
        assert_eq!(usage.prompt_tokens, 16);
        assert_eq!(
            usage.completion_tokens, 50,
            "the answer, without the thinking"
        );
        assert_eq!(
            usage
                .completion_tokens_details
                .expect("reasoning")
                .reasoning_tokens,
            28,
            "thinking is billed at output rates and is counted apart"
        );
        assert_eq!(
            usage.total_tokens, 94,
            "16 + 50 + 28: folding thinking into the answer would have \
             double-counted, leaving it out would have lost it"
        );
        assert_eq!(
            usage.extra["gemini"]["serviceTier"], "standard",
            "the tier multiplies every token on the call"
        );
    }

    /// The tier reaches the chunk rather than only the provider's own object,
    /// because everything downstream reads the canonical shape.
    #[test]
    fn the_service_tier_travels_with_the_chunk() {
        let mut s = GeminiStream::new("gemini-2.5-flash".into());
        let chunk = s
            .event(&serde_json::json!({
                "candidates": [{ "content": { "parts": [{ "text": "hi" }] }, "index": 0 }],
                "usageMetadata": {
                    "promptTokenCount": 16,
                    "candidatesTokenCount": 1,
                    "totalTokenCount": 17,
                    "serviceTier": "standard"
                },
            }))
            .expect("a chunk");
        assert_eq!(chunk.service_tier.as_deref(), Some("standard"));
    }

    /// Gemini signs the *first* function call of a parallel batch and leaves
    /// its siblings unsigned. Measured, not assumed: three calls in one round
    /// came back with a 356-character signature on the first and none on the
    /// other two.
    ///
    /// The temptation is to treat a signature as belonging to the turn and
    /// attach it to every call on replay. Doing that is accepted -- HTTP 200,
    /// no warning -- and bills the previous turn's reasoning once per copy.
    /// The same request replayed as received cost 200 prompt tokens; with the
    /// signature copied onto all three calls it cost 272. Nothing says so at
    /// the time, and the overcharge compounds as the history grows, so the
    /// rule is that a signature belongs to the part it arrived on and is
    /// never moved or duplicated.
    #[test]
    fn a_signature_belongs_to_one_call_and_is_never_copied_to_its_siblings() {
        let mut s = GeminiStream::new("gemini-3.7-flash".into());
        let chunk = s
            .event(&serde_json::json!({
                "candidates": [{
                    "content": { "parts": [
                        {
                            "functionCall": { "name": "get_weather", "args": { "city": "San Francisco" }, "id": "call_1" },
                            "thoughtSignature": "c2lnbmF0dXJl"
                        },
                        { "functionCall": { "name": "get_weather", "args": { "city": "London" }, "id": "call_2" } },
                        { "functionCall": { "name": "get_weather", "args": { "city": "Tokyo" }, "id": "call_3" } }
                    ] },
                    "index": 0,
                }],
            }))
            .expect("three calls in one chunk");

        let calls = chunk.choices[0].delta.tool_calls.as_ref().expect("calls");
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[0].provider_signature.as_deref(),
            Some("c2lnbmF0dXJl"),
            "the signed call keeps its signature"
        );
        assert!(
            calls[1].provider_signature.is_none() && calls[2].provider_signature.is_none(),
            "an unsigned sibling must stay unsigned: copying the first call's \
             signature onto it is accepted by the API and silently bills the \
             previous turn's reasoning again for every copy"
        );
        assert_eq!(
            calls[0].id.as_deref(),
            Some("call_1"),
            "the provider's own id, not one we invented"
        );
        assert_eq!(calls[2].id.as_deref(), Some("call_3"));
    }

    /// Prose is signed at the end of the block, on a part carrying no text.
    ///
    /// Measured against `gemini-3.7-flash`: a 120-word answer arrived in seven
    /// chunks, the first six with words and no signature and the seventh with
    /// an empty string and 3496 characters of signature. A reader that
    /// forwards only parts with something in them -- which is the obvious way
    /// to write it -- throws that away, and the next round is refused for
    /// want of it.
    #[test]
    fn a_text_block_is_signed_at_its_end_by_a_part_with_no_text() {
        let mut s = GeminiStream::new("gemini-3.7-flash".into());

        for word in [
            "San Francisco's fog ",
            "forms when warm air ",
            "meets cold water.",
        ] {
            let chunk = s.event(&serde_json::json!({
                "candidates": [{ "content": { "parts": [{ "text": word }] }, "index": 0 }],
            }));
            assert!(chunk.is_some(), "a part with words in it is forwarded");
        }
        assert!(
            s.text_signature().is_none(),
            "nothing has signed anything yet"
        );

        // The last chunk: a signature, and no text at all.
        s.event(&serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "", "thoughtSignature": "c2lnbmVkLXRleHQ=" }] },
                "finishReason": "STOP",
                "index": 0,
            }],
        }));

        assert_eq!(
            s.text_signature(),
            Some("c2lnbmVkLXRleHQ="),
            "the signature rode a part with an empty string in it, and keeping \
             only parts that say something loses it"
        );
    }

    /// A signature offered once is not unset by the parts that follow it.
    #[test]
    fn a_later_unsigned_part_does_not_clear_a_signature_already_given() {
        let mut s = GeminiStream::new("gemini-3.7-flash".into());
        s.event(&serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "first", "thoughtSignature": "c2ln" }] },
                "index": 0,
            }],
        }));
        assert_eq!(s.text_signature(), Some("c2ln"));

        s.event(&serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": " second" }] }, "index": 0 }],
        }));
        assert_eq!(
            s.text_signature(),
            Some("c2ln"),
            "last-write-wins over every part clears it with the next one that \
             has none, and the next round is then refused"
        );
    }

    /// Version is parsed, not matched as a substring: "gemini-3" occurring
    /// somewhere in a name is a different question from the model being
    /// version three or newer.
    #[test]
    fn a_models_version_is_read_from_its_name() {
        assert_eq!(gemini_major_version("gemini-3.7-flash"), Some(3));
        assert_eq!(gemini_major_version("gemini-2.5-flash"), Some(2));
        assert_eq!(
            gemini_major_version("gemini-10-flash"),
            Some(10),
            "two digits, not one"
        );
        assert_eq!(
            gemini_major_version("gemma-4-31b-it"),
            None,
            "a different family"
        );

        assert!(gemini_wants_call_ids("gemini-3.7-flash"));
        assert!(
            gemini_wants_call_ids("gemini-4-flash"),
            "newer than three is also newer"
        );
        assert!(!gemini_wants_call_ids("gemini-2.5-flash"));
        assert!(
            !gemini_wants_call_ids("my-gemini-30-tune"),
            "a substring test would have said yes to this and to gemini-4 no"
        );
    }

    /// Older models refuse a call id outright, and newer ones match responses
    /// by it, so the field cannot simply be sent to everybody.
    #[test]
    fn call_ids_are_sent_only_to_the_models_that_want_them() {
        let call = ToolCall {
            id: "call_538131".into(),
            tool_type: "function".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"SF"}"#.into(),
            },
            provider_signature: Some("c2ln".into()),
        };
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: MessageContent::Text(String::new()),
                name: None,
                tool_calls: Some(vec![call]),
                tool_call_id: None,
                thinking_blocks: Vec::new(),
                signed_by: None,
            },
            Message {
                role: Role::Tool,
                content: MessageContent::Text("62F and foggy".into()),
                name: Some("get_weather".into()),
                tool_calls: None,
                tool_call_id: Some("call_538131".into()),
                thinking_blocks: Vec::new(),
                signed_by: None,
            },
        ];
        let request = ChatCompletionRequest {
            model: String::new(),
            messages,
            temperature: None,
            max_tokens: None,
            tools: None,
            reasoning_effort: None,
            stream: false,
            stream_options: None,
            facts: None,
        };

        let newer = openai_to_gemini_for(&request, "gemini-3.7-flash").expect("translate");
        assert_eq!(
            newer["contents"][0]["parts"][0]["functionCall"]["id"],
            "call_538131"
        );
        assert_eq!(
            newer["contents"][1]["parts"][0]["functionResponse"]["id"],
            "call_538131"
        );
        assert_eq!(
            newer["contents"][0]["parts"][0]["thoughtSignature"], "c2ln",
            "the signature goes back on the call it came with, or the round is refused"
        );

        let older = openai_to_gemini_for(&request, "gemini-2.5-flash").expect("translate");
        assert!(
            older["contents"][0]["parts"][0]["functionCall"]["id"].is_null(),
            "an older model refuses the field with a 400"
        );
        assert!(older["contents"][1]["parts"][0]["functionResponse"]["id"].is_null());
        assert_eq!(
            older["contents"][0]["parts"][0]["thoughtSignature"], "c2ln",
            "it still signs, and still demands the signature back"
        );
    }

    /// The runtime's results say which call they answer and not which
    /// function, as this protocol allows. Gemini refuses a response without a
    /// name, so the call's is used.
    #[test]
    fn a_result_without_a_name_takes_its_calls() {
        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": "", "tool_calls": [{
                    "id": "c1", "type": "function", "provider_signature": "c2ln",
                    "function": {"name": "get_weather", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "fog"},
            ],
        }))
        .expect("request");
        let body = openai_to_gemini_for(&request, "gemini-3.7-flash").expect("translate");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "get_weather"
        );
    }

    /// A call another model signed is as good as unsigned to this one, which
    /// would refuse it: the exchange is rendered instead of replayed.
    #[test]
    fn a_call_signed_by_another_model_is_rendered() {
        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": "", "signed_by": "gemini-3.5-pro",
                 "tool_calls": [{"id": "c1", "type": "function", "provider_signature": "c2ln",
                                 "function": {"name": "get_weather", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "fog"},
            ],
        }))
        .expect("request");
        let elsewhere = openai_to_gemini_for(&request, "gemini-3.7-flash")
            .expect("translate")
            .to_string();
        assert!(!elsewhere.contains("functionCall"), "{elsewhere}");
        let here = openai_to_gemini_for(&request, "gemini-3.5-pro")
            .expect("translate")
            .to_string();
        assert!(here.contains("thoughtSignature"), "{here}");
    }

    /// A conversation that changed models has tool calls whose signatures
    /// belong to a model no longer answering. Replaying them natively is
    /// refused, so the exchange is rendered as prose: every byte of what
    /// happened, moved somewhere no validation applies.
    #[test]
    fn an_unsigned_tool_turn_is_rendered_rather_than_replayed() {
        let request = ChatCompletionRequest {
            model: String::new(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: MessageContent::Text(String::new()),
                    name: None,
                    tool_calls: Some(vec![ToolCall {
                        provider_signature: None,
                        id: "c1".into(),
                        tool_type: "function".into(),
                        function: FunctionCall {
                            name: "get_weather".into(),
                            arguments: r#"{"city":"SF"}"#.into(),
                        },
                    }]),
                    tool_call_id: None,
                    thinking_blocks: Vec::new(),
                    signed_by: None,
                },
                Message {
                    role: Role::Tool,
                    content: MessageContent::Text("62F and foggy".into()),
                    name: Some("get_weather".into()),
                    tool_calls: None,
                    tool_call_id: Some("c1".into()),
                    thinking_blocks: Vec::new(),
                    signed_by: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            tools: None,
            reasoning_effort: None,
            stream: false,
            stream_options: None,
            facts: None,
        };

        let json = openai_to_gemini_for(&request, "gemini-3.7-flash")
            .expect("translate")
            .to_string();

        assert!(
            !json.contains("functionCall"),
            "an unsigned call replayed as a call is refused with a 400"
        );
        assert!(
            !json.contains("functionResponse"),
            "and a response to a call the model was never shown answers nothing"
        );
        assert!(
            json.contains("get_weather"),
            "what was asked for is still said"
        );
        assert!(json.contains("62F and foggy"), "and so is what came back");
        assert!(
            !json.contains("skip_thought_signature_validator"),
            "no sentinel: asking a provider to skip a check on data known to be \
             invalid costs reasoning quality and nothing reports it"
        );
    }

    /// An over-long result keeps both ends, on the same reasoning as the
    /// agent's truncator: the start says what a thing is and the end is often
    /// the answer, while the middle rarely decides anything.
    #[test]
    fn an_over_long_result_keeps_both_ends() {
        let result = format!("BEGINNING{}END", "x".repeat(MAX_FLATTENED_BYTES * 2));
        let rendered = flatten_tool_exchange("q", "{}", &result);

        assert!(
            rendered.len() < MAX_FLATTENED_BYTES + 256,
            "the budget is a budget"
        );
        assert!(rendered.contains("BEGINNING"), "the start says what it is");
        assert!(rendered.contains("END"), "the end is often the answer");
        assert!(
            rendered.contains("elided"),
            "and the cut is admitted, with its size"
        );
    }

    /// Cutting through a multi-byte character would panic.
    #[test]
    fn a_cut_lands_on_a_character_boundary() {
        let rendered = flatten_tool_exchange("q", "{}", &"e\u{301}".repeat(MAX_FLATTENED_BYTES));
        assert!(rendered.contains("elided"));
    }

    /// Gemini refuses a whole request when a tool's schema carries a keyword
    /// it does not model, naming a path several levels deep and taking the
    /// turn down with it. Measured against the real thing: a tool declaring
    /// `additionalProperties` for a map of headers -- which is how every
    /// other provider expects it -- came back 400 INVALID_ARGUMENT.
    #[test]
    fn a_tool_schema_is_reduced_to_what_gemini_will_accept() {
        // The declaration that actually failed, as the agent writes it.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Absolute https URL." },
                "headers": {
                    "type": "object",
                    "description": "Extra headers, as a flat object.",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["url"]
        });

        let reduced = gemini_schema(&schema);
        assert!(
            !reduced.to_string().contains("additionalProperties"),
            "the keyword that returns a 400 survived: {reduced}"
        );
        // Everything the model needs to call the tool is still there.
        assert_eq!(reduced["type"], "object");
        assert_eq!(reduced["properties"]["url"]["type"], "string");
        assert_eq!(
            reduced["properties"]["headers"]["description"], "Extra headers, as a flat object.",
            "stripping went further than the keywords it was meant to remove"
        );
        assert_eq!(reduced["required"][0], "url");
    }

    /// A system message is its own field here, and repeated ones are joined
    /// rather than the last one quietly winning.
    #[test]
    fn system_messages_become_one_instruction() {
        let request = ChatCompletionRequest {
            model: "gemini-test".into(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: MessageContent::Text("be brief".into()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking_blocks: Vec::new(),
                    signed_by: None,
                },
                Message {
                    role: Role::System,
                    content: MessageContent::Text("be kind".into()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking_blocks: Vec::new(),
                    signed_by: None,
                },
                Message {
                    role: Role::User,
                    content: MessageContent::Text("hello".into()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking_blocks: Vec::new(),
                    signed_by: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            tools: None,
            reasoning_effort: None,
            stream: false,
            stream_options: None,
            facts: None,
        };

        let body = openai_to_gemini(&request).expect("translate");
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"], "be brief\nbe kind",
            "a dropped system message silently changes what was asked"
        );
        assert_eq!(body["contents"].as_array().expect("contents").len(), 1);
        assert_eq!(body["contents"][0]["role"], "user");
    }

    /// The effort asked for becomes Gemini's thinking level, clamped to the
    /// levels the model offers, and nothing is sent for a model with none.
    #[test]
    fn an_effort_becomes_a_thinking_level_the_model_offers() {
        let ask = |effort: &str, offered: &[&str]| {
            let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
                "model": "gemini-x",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": effort,
            }))
            .expect("request");
            let mut request = request;
            request.facts = Some(std::sync::Arc::new(outturn_core::facts::ModelFacts {
                efforts: offered.iter().map(|e| e.to_string()).collect(),
                ..Default::default()
            }));
            openai_to_gemini(&request).expect("translate")["generationConfig"]["thinkingConfig"]
                ["thinkingLevel"]
                .clone()
        };
        let flash = ["minimal", "low", "medium", "high"];
        let pro = ["low", "medium", "high"];
        assert_eq!(ask("medium", &flash), "medium");
        assert_eq!(ask("none", &flash), "minimal");
        assert_eq!(ask("none", &pro), "low");
        assert_eq!(ask("max", &pro), "high");
        assert_eq!(ask("medium", &["low", "high"]), serde_json::Value::Null);
        assert_eq!(ask("high", &[]), serde_json::Value::Null);
    }

    /// The assistant is called "model" here, and a turn that used tools has
    /// to be replayable or the next round loses its own history.
    #[test]
    fn an_assistant_turn_with_a_tool_call_replays_as_a_model_turn() {
        let request = ChatCompletionRequest {
            model: "gemini-test".into(),
            messages: vec![Message {
                role: Role::Assistant,
                content: MessageContent::Text(String::new()),
                name: None,
                tool_calls: Some(vec![ToolCall {
                    // Signed, because that is the case where a call replays as
                    // a call; an unsigned one is rendered as prose instead.
                    provider_signature: Some("c2ln".into()),
                    id: "c1".into(),
                    tool_type: "function".into(),
                    function: FunctionCall {
                        name: "lookup".into(),
                        arguments: r#"{"city":"SF"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                thinking_blocks: Vec::new(),
                signed_by: None,
            }],
            temperature: None,
            max_tokens: None,
            tools: None,
            reasoning_effort: None,
            stream: false,
            stream_options: None,
            facts: None,
        };

        let body = openai_to_gemini(&request).expect("translate");
        assert_eq!(body["contents"][0]["role"], "model", "not 'assistant' here");
        let call = &body["contents"][0]["parts"][0]["functionCall"];
        assert_eq!(call["name"], "lookup");
        assert_eq!(
            call["args"]["city"], "SF",
            "arguments are an object here and a string in the other protocol"
        );
    }
}

/// Each vendor's whole response and its stream, made to say the same thing
/// and compared. The two paths read the same blocks and parts; where they
/// still differ, the difference is asserted here as it stands, so changing
/// it is a decision rather than an accident.
#[cfg(test)]
mod path_agreement_tests {
    use super::*;
    use serde_json::json;

    /// A tool call as both paths can be read for it: id, name, arguments,
    /// signature.
    type Call = (String, String, String, Option<String>);

    /// What a stream's chunks add up to, read the way a consumer reads them.
    #[derive(Debug, Default)]
    struct Assembled {
        content: String,
        reasoning: String,
        thinking_blocks: Vec<serde_json::Value>,
        calls: Vec<Call>,
        finish_reason: Option<String>,
        refusal: Option<String>,
    }

    fn assemble(chunks: impl IntoIterator<Item = StreamChunk>) -> Assembled {
        let mut out = Assembled::default();
        for chunk in chunks {
            let choice = &chunk.choices[0];
            let d = &choice.delta;
            out.content
                .push_str(d.content.as_deref().unwrap_or_default());
            out.reasoning
                .push_str(d.reasoning.as_deref().unwrap_or_default());
            out.thinking_blocks.extend(d.thinking_block.clone());
            for call in d.tool_calls.iter().flatten() {
                let slot = call.index as usize;
                if out.calls.len() <= slot {
                    out.calls.resize(slot + 1, Default::default());
                }
                let entry = &mut out.calls[slot];
                if let Some(id) = &call.id {
                    entry.0 = id.clone();
                }
                if let Some(f) = &call.function {
                    if let Some(name) = &f.name {
                        entry.1 = name.clone();
                    }
                    entry.2.push_str(f.arguments.as_deref().unwrap_or_default());
                }
                if call.provider_signature.is_some() {
                    entry.3 = call.provider_signature.clone();
                }
            }
            if choice.finish_reason.is_some() {
                out.finish_reason = choice.finish_reason.clone();
            }
            if d.refusal.is_some() {
                out.refusal = d.refusal.clone();
            }
        }
        out
    }

    fn calls_of(resp: &ChatCompletionResponse) -> Vec<Call> {
        resp.choices[0]
            .message
            .tool_calls
            .iter()
            .flatten()
            .map(|c| {
                (
                    c.id.clone(),
                    c.function.name.clone(),
                    c.function.arguments.clone(),
                    c.provider_signature.clone(),
                )
            })
            .collect()
    }

    fn text_of(resp: &ChatCompletionResponse) -> String {
        match &resp.choices[0].message.content {
            MessageContent::Text(t) => t.clone(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// One Anthropic reply, whole and as the events that stream it.
    fn anthropic_both(
        content: serde_json::Value,
        events: Vec<serde_json::Value>,
        stop_reason: &str,
        stop_details: serde_json::Value,
    ) -> (ChatCompletionResponse, Assembled) {
        let whole = anthropic_to_openai(&json!({
            "id": "msg_1", "model": "claude-test",
            "content": content,
            "stop_reason": stop_reason,
            "stop_details": stop_details,
            "usage": {"input_tokens": 10, "output_tokens": 5},
        }))
        .expect("translate");
        let mut stream = AnthropicStream::new();
        let mut all = vec![json!({
            "type": "message_start",
            "message": {"id": "msg_1", "model": "claude-test",
                        "usage": {"input_tokens": 10, "output_tokens": 1}},
        })];
        all.extend(events);
        all.push(json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_details": stop_details},
            "usage": {"output_tokens": 5},
        }));
        all.push(json!({"type": "message_stop"}));
        let streamed = assemble(all.iter().filter_map(|e| stream.event(e)));
        (whole, streamed)
    }

    #[test]
    fn anthropic_text_and_a_split_tool_call_agree() {
        let (whole, streamed) = anthropic_both(
            json!([
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "id": "toolu_1", "name": "search",
                 "input": {"q": "x"}},
            ]),
            vec![
                json!({"type": "content_block_start", "index": 0,
                       "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "text_delta", "text": "Look"}}),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "text_delta", "text": "ing."}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "content_block_start", "index": 1,
                       "content_block": {"type": "tool_use", "id": "toolu_1",
                                         "name": "search", "input": {}}}),
                json!({"type": "content_block_delta", "index": 1,
                       "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}}),
                json!({"type": "content_block_delta", "index": 1,
                       "delta": {"type": "input_json_delta", "partial_json": "\"x\"}"}}),
                json!({"type": "content_block_stop", "index": 1}),
            ],
            "tool_use",
            serde_json::Value::Null,
        );
        assert_eq!(text_of(&whole), streamed.content);
        assert_eq!(calls_of(&whole), streamed.calls);
        assert_eq!(whole.choices[0].finish_reason, streamed.finish_reason);
        assert_eq!(streamed.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn anthropic_max_tokens_agrees() {
        let (whole, streamed) = anthropic_both(
            json!([{"type": "text", "text": "cut"}]),
            vec![
                json!({"type": "content_block_start", "index": 0,
                       "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "text_delta", "text": "cut"}}),
            ],
            "max_tokens",
            serde_json::Value::Null,
        );
        assert_eq!(text_of(&whole), streamed.content);
        assert_eq!(whole.choices[0].finish_reason, streamed.finish_reason);
        assert_eq!(streamed.finish_reason.as_deref(), Some("length"));
    }

    /// Disagreement, kept: the stream forwards thinking as `reasoning` and
    /// the signed block for replay, and the whole response drops both.
    #[test]
    fn anthropic_thinking_reaches_only_the_stream() {
        let (whole, streamed) = anthropic_both(
            json!([
                {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                {"type": "text", "text": "ok"},
            ]),
            vec![
                json!({"type": "content_block_start", "index": 0,
                       "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "signature_delta", "signature": "sig"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "content_block_start", "index": 1,
                       "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 1,
                       "delta": {"type": "text_delta", "text": "ok"}}),
            ],
            "end_turn",
            serde_json::Value::Null,
        );
        assert_eq!(text_of(&whole), streamed.content);
        assert_eq!(streamed.reasoning, "hmm");
        assert_eq!(
            streamed.thinking_blocks,
            vec![json!({"type": "thinking", "thinking": "hmm", "signature": "sig"})]
        );
        assert!(whole.choices[0].message.thinking_blocks.is_empty());
        assert_eq!(whole.choices[0].parts.len(), 1, "only the text");
    }

    /// The finish reason agrees; the explanation reaches only the stream,
    /// since the whole response has no field for it.
    #[test]
    fn anthropic_refusal_agrees_on_its_finish_reason() {
        let details = json!({"explanation": "Not this. More words.", "category": "cyber"});
        let (whole, streamed) = anthropic_both(json!([]), vec![], "refusal", details);
        assert_eq!(whole.choices[0].finish_reason, streamed.finish_reason);
        assert_eq!(streamed.finish_reason.as_deref(), Some("content_filter"));
        assert_eq!(streamed.refusal.as_deref(), Some("Not this."));
    }

    /// One Gemini reply: the whole response is the chunks' parts in one
    /// candidate, with the last chunk's finish reason.
    fn gemini_both(
        chunks: Vec<serde_json::Value>,
        finish: &str,
    ) -> (ChatCompletionResponse, Assembled) {
        let parts: Vec<serde_json::Value> = chunks
            .iter()
            .flat_map(|c| c.as_array().cloned().unwrap_or_default())
            .collect();
        let mut candidate = json!({"content": {"parts": parts}, "finishReason": finish});
        if finish == "SAFETY" {
            candidate["finishMessage"] = json!("Blocked.");
        }
        let whole = gemini_to_openai(&json!({
            "candidates": [candidate], "modelVersion": "gemini-test",
        }))
        .expect("translate");
        let mut stream = GeminiStream::new("gemini-test".to_string());
        let n = chunks.len();
        let streamed = assemble(chunks.into_iter().enumerate().filter_map(|(i, parts)| {
            let mut c = json!({"content": {"parts": parts}});
            if i + 1 == n {
                c["finishReason"] = json!(finish);
                if finish == "SAFETY" {
                    c["finishMessage"] = json!("Blocked.");
                }
            }
            stream.event(&json!({"candidates": [c]}))
        }));
        (whole, streamed)
    }

    #[test]
    fn gemini_text_and_calls_agree() {
        let (whole, streamed) = gemini_both(
            vec![
                json!([{"text": "Look"}]),
                json!([{"text": "ing."}]),
                json!([
                    {"functionCall": {"name": "search", "args": {"q": "x"}},
                     "thoughtSignature": "sig"},
                    {"functionCall": {"id": "own", "name": "fetch", "args": {}}},
                ]),
            ],
            "STOP",
        );
        assert_eq!(text_of(&whole), streamed.content);
        assert_eq!(calls_of(&whole), streamed.calls);
        assert_eq!(streamed.calls[0].0, "gemini_call_0");
        assert_eq!(whole.choices[0].finish_reason, streamed.finish_reason);
        assert_eq!(streamed.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn gemini_max_tokens_agrees() {
        let (whole, streamed) = gemini_both(vec![json!([{"text": "cut"}])], "MAX_TOKENS");
        assert_eq!(text_of(&whole), streamed.content);
        assert_eq!(whole.choices[0].finish_reason, streamed.finish_reason);
        assert_eq!(streamed.finish_reason.as_deref(), Some("length"));
    }

    /// Disagreement, kept: the stream says why the provider declined, and
    /// the whole response carries only the finish reason.
    #[test]
    fn gemini_refusal_agrees_on_its_finish_reason() {
        let (whole, streamed) = gemini_both(vec![json!([])], "SAFETY");
        assert_eq!(whole.choices[0].finish_reason, streamed.finish_reason);
        assert_eq!(streamed.finish_reason.as_deref(), Some("content_filter"));
        assert_eq!(streamed.refusal.as_deref(), Some("Blocked."));
    }

    /// Disagreement, kept: a signature on a text part is held by the stream
    /// for the next round, and the whole response has nowhere it goes.
    #[test]
    fn gemini_text_signature_is_kept_only_by_the_stream() {
        let parts = json!([{"text": "hi"}, {"text": "", "thoughtSignature": "tsig"}]);
        let mut stream = GeminiStream::new("g".to_string());
        stream.event(&json!({"candidates": [{"content": {"parts": parts}}]}));
        assert_eq!(stream.text_signature(), Some("tsig"));
        let whole = gemini_to_openai(&json!({"candidates": [
            {"content": {"parts": parts}, "finishReason": "STOP"},
        ]}))
        .expect("translate");
        assert_eq!(text_of(&whole), "hi");
        assert_eq!(
            whole.choices[0].parts.len(),
            2,
            "an empty part is still a part"
        );
    }
}
