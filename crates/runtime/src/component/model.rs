//! Model calls: a round of the conversation, and a look at an image, each
//! counted against the turn's rounds and billed as it returns.

use std::sync::Arc;

use super::stream::{Served, stream_completion};
use super::{
    AgentHost, CallUsage, Completion, CompletionRequest, ContentPart, MalformedReply, ProgressSink,
    ROUND_SEPARATOR, Refused, Signed, ToolCall, Usage,
};

/// The OpenAI-shaped projection of an ordered message: the text joined, the
/// calls listed after it.
///
/// That protocol has no way to say a call came between two pieces of text, so
/// anything sent over it loses the arrangement. The parts travel beside it
/// under `outturn.parts` for providers that can say it -- see
/// `openai_to_anthropic` in the gateway.
pub(crate) fn flatten_parts(parts: &[ContentPart]) -> (String, Vec<ToolCall>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text(t) => text.push_str(t),
            ContentPart::Call(c) => calls.push(c.clone()),
        }
    }
    (text, calls)
}

/// What a model call made after the turn was stopped is told.
const STOPPED: &str = "this turn has been stopped; no more model calls may be made in it";

impl AgentHost {
    /// Counts a model call against the turn's limit, before it is made.
    ///
    /// Before rather than after: the limit is on what may be spent, and a
    /// guest that ignores what `limits` told it is refused here rather than
    /// politely asked again. Every paid call takes a round -- a look at an
    /// image as much as a round of the conversation -- or a model looping on
    /// one of them is unbounded on the other.
    ///
    /// A stopped turn is refused here too. The default guest checks the flag
    /// between rounds, but a component is workspace code: one that ignores it
    /// would otherwise go on spending after somebody pressed stop.
    pub(super) fn take_round(&mut self) -> Result<(), String> {
        if self.cancelled {
            tracing::warn!(
                session_id = %self.session_id,
                "guest asked for a model call after its turn was stopped"
            );
            return Err(STOPPED.into());
        }
        self.rounds_used += 1;
        if self.max_tool_rounds > 0 && self.rounds_used > self.max_tool_rounds {
            tracing::warn!(
                session_id = %self.session_id,
                limit = self.max_tool_rounds,
                "guest exceeded its round limit"
            );
            return Err(format!(
                "round limit reached: this turn may call the model {} times",
                self.max_tool_rounds
            ));
        }
        Ok(())
    }

    /// Bills one call, the moment its cost is known.
    ///
    /// Per call rather than summed at the end, so a turn that fails after
    /// three calls still bills for three. `rounds_used` was counted before
    /// the call, so the round is one less.
    pub(super) fn bill(
        &mut self,
        traffic_type: &str,
        asked_for: &str,
        served: &Served,
        usage: Option<&Usage>,
    ) {
        let Some(usage) = usage else {
            return;
        };
        if let Some(sink) = &self.on_usage {
            sink(&CallUsage {
                round: self.rounds_used.saturating_sub(1),
                traffic_type: traffic_type.to_string(),
                endpoint: served.endpoint.clone().unwrap_or_default(),
                model: served
                    .model
                    .clone()
                    .unwrap_or_else(|| asked_for.to_string()),
                paid_by: served
                    .paid_by
                    .clone()
                    .unwrap_or_else(|| "operator".to_string()),
                usage: *usage,
                provider_usage: served.provider_usage.clone(),
                service_tier: served.service_tier.clone(),
                partial: served.partial,
                estimated: served.usage_estimated,
            });
        }
        // Summed across rounds: a turn's cost is every call it made, not the
        // last one. A provider that reports nothing simply adds nothing.
        self.spent.prompt_tokens += usage.prompt_tokens;
        self.spent.completion_tokens += usage.completion_tokens;
        self.spent.cache_read_tokens += usage.cache_read_tokens;
        self.spent.cache_write_tokens += usage.cache_write_tokens;
        self.spent.reasoning_tokens += usage.reasoning_tokens;
    }

    pub(super) async fn chat(&mut self, request: CompletionRequest) -> Result<Completion, String> {
        self.take_round()?;

        // The credential is attached here rather than being handed to the
        // guest, so a compromised component can spend this session's allowance
        // but cannot take the token elsewhere.
        let model = request.model.unwrap_or_else(|| self.default_model.clone());

        let messages = request
            .messages
            .iter()
            .map(|m| {
                let (content, tool_calls) = flatten_parts(&m.parts);
                let mut value = serde_json::json!({
                    "role": m.role,
                    "content": content,
                });
                // Sent beside the flattening, not instead of it: a provider
                // that can hold the order gets it, and one that cannot reads
                // the fields it already understands and ignores this.
                if m.parts.len() > 1 {
                    value["outturn"] = serde_json::json!({
                        "parts": m.parts.iter().map(|p| match p {
                            ContentPart::Text(t) => serde_json::json!({"type": "text", "text": t}),
                            ContentPart::Call(c) => serde_json::json!({
                                "type": "tool_call",
                                "id": c.id,
                                "name": c.name,
                                "arguments": c.arguments,
                            }),
                        }).collect::<Vec<_>>()
                    });
                }
                if !tool_calls.is_empty() {
                    value["tool_calls"] = serde_json::json!(
                        tool_calls
                            .iter()
                            .map(|c| serde_json::json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.arguments },
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                if let Some(id) = &m.tool_call_id {
                    value["tool_call_id"] = serde_json::json!(id);
                }
                // With the model that signed them named beside them. Which model
                // may read them is the gateway's question -- it alone knows
                // which one this round goes to.
                if let Some(signed) = tool_calls.first().and_then(|c| self.signed.get(&c.id)) {
                    if !signed.model.is_empty() {
                        value["signed_by"] = serde_json::json!(signed.model);
                    }
                    if !signed.thinking.is_empty() {
                        value["thinking_blocks"] = serde_json::json!(signed.thinking);
                    }
                    if let Some(calls) = value["tool_calls"].as_array_mut() {
                        for call in calls {
                            if let Some(signature) =
                                call["id"].as_str().and_then(|id| signed.calls.get(id))
                            {
                                call["provider_signature"] = serde_json::json!(signature);
                            }
                        }
                    }
                }
                value
            })
            .collect::<Vec<_>>();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            // The guest's choice if it made one, else the platform's.
            "temperature": request.temperature.or(self.temperature),
            "max_tokens": request.max_tokens,
        });

        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = serde_json::json!(effort);
        }

        // Omitted rather than sent empty: offering no tools is the common
        // case, and some providers reject an empty array.
        if !request.tools.is_empty() {
            body["tools"] = serde_json::json!(
                request
                    .tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            // Schemas cross as text because WIT has no JSON
                            // type. Unparseable ones degrade to an empty
                            // object, so a malformed schema costs the tool its
                            // arguments rather than failing the whole turn.
                            "parameters": serde_json::from_str::<serde_json::Value>(&t.parameters)
                                .unwrap_or_else(|_| serde_json::json!({"type": "object"})),
                        },
                    }))
                    .collect::<Vec<_>>()
            );
        }

        // Rounds of one turn are separated by a paragraph break, and the
        // break has to reach the browser *before* the round's first token.
        // The guest cannot put it there: it learns a round produced text
        // only when `chat` returns, by which time the text has streamed. So
        // the host inserts it here, at the first token, when an earlier
        // round has already shown something -- and the guest joins rounds
        // with the same "\n\n" when it assembles the reply, so what streamed
        // and what is stored stay the same string.
        let progress = self.progress.as_ref().map(|sink| {
            let sink = Arc::clone(sink);
            let separate = self.streamed;
            let pending = std::sync::atomic::AtomicBool::new(separate);
            Arc::new(move |text: &str| {
                if pending.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    sink(ROUND_SEPARATOR);
                }
                sink(text);
            }) as ProgressSink
        });

        // The request, as a value and as the bytes sent, held for the call;
        // the reply is charged as it arrives, by `stream_completion`.
        let _request = self.charge(
            (body.to_string().len() as u64).saturating_mul(2),
            "call the model",
        )?;
        let token = self.current_token().await;
        let (completion, arrivals, served) = stream_completion(
            &self.http,
            &self.gateway_url,
            &token,
            &self.traffic_type,
            &self.reply_id,
            body,
            progress.as_ref(),
            self.reasoning.as_ref(),
            self.writing.as_ref(),
            self.admission.as_deref(),
        )
        .await
        .map_err(|e| {
            self.malformed_reply |= e.downcast_ref::<MalformedReply>().is_some();
            if let Some(Refused(refusal)) = e.downcast_ref::<Refused>() {
                self.refused = Some(refusal.clone());
            }
            e.to_string()
        })?;

        if completion
            .parts
            .iter()
            .any(|p| matches!(p, ContentPart::Text(t) if !t.is_empty()))
        {
            self.streamed = true;
        }

        let traffic_type = self.traffic_type.clone();
        self.bill(&traffic_type, &model, &served, completion.usage.as_ref());
        let served_by = served.endpoint;
        if let Some(ContentPart::Call(first)) = completion
            .parts
            .iter()
            .find(|p| matches!(p, ContentPart::Call(_)))
            && (!served.signed.thinking.is_empty() || !served.signed.calls.is_empty())
        {
            let signed = Signed {
                first_call: first.id.clone(),
                model: served.model.clone().unwrap_or_default(),
                ..served.signed
            };
            self.signed.insert(first.id.clone(), signed);
        }

        // Buffered rather than delivered: the guest asks at a boundary it
        // chooses, which is the only point where injecting a message does not
        // corrupt a round already in flight.
        self.arrivals.extend(arrivals);
        // Once asked, always asked. A later round that says nothing about it
        // is not a withdrawal, and the guest may not look until the round
        // after the one that carried the news.
        self.cancelled |= served.cancelled;
        // Sticky for the same reason: a later round saying nothing about it is
        // not a withdrawal.
        if self.held.is_none() {
            self.held = served.held.clone();
        }

        if served_by.is_some() {
            self.served_by = served_by;
        }
        if served.model.is_some() {
            self.served_model = served.model;
        }

        Ok(completion)
    }

    /// Asks a model that can see what is in a stored image.
    ///
    /// The bytes are read here and never handed over. A guest that could hold
    /// an image could send it somewhere through the one channel it has, and
    /// what it wants is the answer rather than the pixels -- the same trade
    /// `read_object` already makes for a document.
    pub(super) async fn describe_image(
        &mut self,
        path: String,
        question: String,
    ) -> Result<String, String> {
        use outturn_core::vision;

        let (storage, resolved) = self.object_at(&path)?;

        // What the file is and how large, before anything is charged for it:
        // a file that could never be shown is answered as that, whatever the
        // pod's memory, rather than as a shortage that waiting will not fix.
        let size = storage
            .stat(&resolved)
            .await
            .map_err(|e| format!("could not read {path}: {e}"))?
            .size;
        if size >= vision::MAX_IMAGE_BYTES as u64 {
            return Err(format!(
                "{path} is too large to show a model; the limit is {}MB",
                vision::MAX_IMAGE_BYTES / (1024 * 1024)
            ));
        }
        // Sniffed rather than trusted from the name: an extension is a claim
        // by whoever uploaded the file, and a provider told the wrong type
        // refuses in a way that reads like a fault here.
        let head = storage
            .read(&resolved, 0, 16)
            .await
            .map_err(|e| format!("could not read {path}: {e}"))?;
        let Some(media_type) = vision::media_type(&head) else {
            return Err(format!(
                "{path} is not an image this can show a model (PNG, JPEG, GIF and WebP are)"
            ));
        };

        // The image, its base64 in the request, and the request as sent: about
        // three and a third times the image, held until the answer is back.
        let _charge = self.charge(size.saturating_mul(10) / 3, "look at an image")?;

        let bytes = storage
            .read(&resolved, 0, vision::MAX_IMAGE_BYTES as u32)
            .await
            .map_err(|e| format!("could not read {path}: {e}"))?;

        // Checked again on what was read: the file can be replaced between
        // the look above and this read, and what was charged for is the size
        // seen then.
        if bytes.len() as u64 > size || vision::media_type(&bytes) != Some(media_type) {
            return Err(format!("{path} changed while it was being read; try again"));
        }

        let encoded = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        };

        // A `vision` route names the model that looks, and overrides what is
        // sent here. This turn's own model is the fallback, for the deployment
        // that has configured no route -- which is the common local case,
        // where the model holding the conversation can see perfectly well. A
        // route is how a deployment says otherwise.
        let body = vision::request(&self.default_model, media_type, &encoded, &question);

        // A paid call like any round of the conversation, and accounted for
        // the same way: it takes a round, so a model looping on an image is
        // bounded, and it is billed, so what a picture cost reaches the
        // ledger. Made through the streaming path so the gateway names who
        // served it and what it cost, exactly as it does for a round; nothing
        // is streamed to the reader, since the answer is the guest's to use.
        self.take_round()?;
        let token = self.current_token().await;
        let (completion, arrivals, served) = stream_completion(
            &self.http,
            &self.gateway_url,
            &token,
            vision::TRAFFIC_TYPE,
            &self.reply_id,
            body,
            None,
            None,
            None,
            self.admission.as_deref(),
        )
        .await
        // Said plainly rather than dressed up: a deployment with no vision
        // route configured lands here, and the agent should say so instead of
        // guessing at the picture. A refusal is the tool's answer too, rather
        // than the turn's end: the conversation was not refused, one look was.
        .map_err(|e| format!("a vision model was not available: {e}"))?;
        self.bill(
            vision::TRAFFIC_TYPE,
            &self.default_model.clone(),
            &served,
            completion.usage.as_ref(),
        );
        // What arrived during the look is kept for the guest as it would be
        // from a round, and a stop asked for during it is still a stop.
        self.arrivals.extend(arrivals);
        self.cancelled |= served.cancelled;
        if self.held.is_none() {
            self.held = served.held.clone();
        }

        let text = completion
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<String>()
            .trim()
            .to_string();

        if text.is_empty() {
            return Err("the model looked and said nothing".to_string());
        }
        Ok(text)
    }
}
