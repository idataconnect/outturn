//! The platform's default agent.
//!
//! Runs a conversational turn: hands the conversation to the model, runs any
//! tool the model asks for, and returns its reply. Deliberately minimal -- it
//! exists to make the runtime tier real, and to be the reference for what a
//! guest looks like.
//!
//! Everything this component can reach is a host import. It has no sockets, no
//! filesystem and no credentials: a model call goes through `chat`, the clock
//! comes from `current-time`, and the host attaches the token.

#[allow(warnings)]
mod bindings;

use bindings::exports::outturn::agent::agent::Guest;
use bindings::outturn::agent::host::{
    self, Arrival, CompletionRequest, Message, ToolActivity, ToolCall, ToolDefinition,
};

struct Component;

/// The model's name for the clock tool.
const CURRENT_TIME: &str = "get_current_time";

/// The argument every tool carries so the user can see what is happening.
///
/// Asked of the model rather than inferred: only the model knows what it is
/// about to do, and a label written by the guest would be a plausible fiction
/// attributed to the agent.
const ACTION: &str = "action";

fn tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: CURRENT_TIME.to_string(),
        description: "The current date and time in the user's own timezone. \
                      Call this whenever the answer depends on what time it is \
                      now -- today's date, the day of the week, how long until \
                      something. Do not guess it."
            .to_string(),
        // No timezone argument: the zone is the user's, and letting the
        // model name one would invite it to invent a plausible wrong answer.
        // `action` is the one thing asked of it, and it is for the user.
        parameters: r#"{"type":"object","properties":{"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Checking today's date. Not an explanation of why."}},"required":["action"]}"#
            .to_string(),
    }]
}

/// Turns what the user said mid-turn into messages for the model.
///
/// Marked as having arrived during the work, because that is true and the
/// model should treat it as a correction to what it is doing rather than as
/// the next question in an orderly exchange.
fn injected(arrivals: &[Arrival]) -> Vec<Message> {
    arrivals
        .iter()
        .map(|arrival| Message {
            role: "user".to_string(),
            content: format!("[mid-turn message from user] {}", arrival.content),
            tool_calls: Vec::new(),
            tool_call_id: None,
        })
        .collect()
}

/// Runs one tool call and returns the message answering it.
fn run_tool(call: &ToolCall) -> Message {
    let content = match call.name.as_str() {
        CURRENT_TIME => {
            let clock = host::current_time();
            // The weekday is given rather than left to be worked out: a model
            // doing calendar arithmetic on a date gets it wrong often enough
            // to matter, and the host already knows the answer exactly.
            format!(
                r#"{{"now":"{}","weekday":"{}","timezone":"{}","abbreviation":"{}"}}"#,
                clock.now,
                clock.weekday,
                if clock.timezone.is_empty() { "UTC" } else { &clock.timezone },
                clock.abbreviation,
            )
        }
        // Reported to the model rather than failing the turn: it can recover
        // by answering without the tool, where an error ends the conversation.
        other => format!(r#"{{"error":"no such tool: {other}"}}"#),
    };

    Message {
        role: "tool".to_string(),
        content,
        tool_calls: Vec::new(),
        tool_call_id: Some(call.id.clone()),
    }
}

/// Separates the user-facing label from the arguments the model gets back.
///
/// Returns the label and the arguments without it. Unparseable arguments are
/// passed through untouched: the model wrote them, and a tool that rejects
/// them gives a better error than the guest silently rewriting them.
fn split_action(arguments: &str) -> (String, String) {
    let Ok(serde_json::Value::Object(mut object)) =
        serde_json::from_str::<serde_json::Value>(arguments)
    else {
        return (String::new(), arguments.to_string());
    };

    let action = match object.remove(ACTION) {
        Some(serde_json::Value::String(action)) => action,
        _ => String::new(),
    };
    let rest = serde_json::to_string(&object).unwrap_or_else(|_| "{}".to_string());
    (action, rest)
}

impl Guest for Component {
    fn run(conversation: Vec<Message>, system_prompt: String) -> Result<String, String> {
        // The system prompt leads the conversation rather than being stored
        // with it, so editing an agent takes effect on its next turn instead
        // of only on new sessions.
        let mut messages = Vec::with_capacity(conversation.len() + 1);
        if !system_prompt.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: system_prompt,
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
        messages.extend(conversation);

        if messages.iter().all(|m| m.role != "user") {
            return Err("conversation contains nothing to respond to".to_string());
        }

        // Everything the model says across every round, which is exactly
        // what the user watched arrive. The host streams each call as it
        // happens and cannot take it back, so the reply has to account for
        // all of it or the browser ends up holding text the transcript does
        // not have.
        let mut reply = String::new();

        // Asked of the host rather than decided here: the limit is the
        // platform's to set, and the host enforces it whether or not a guest
        // bothers to look. Reading it is what allows a graceful ending instead
        // of being refused mid-loop.
        let max_rounds = host::current_limits().max_tool_rounds;

        let mut round: u32 = 0;
        loop {
            // On the last permitted round tools are withheld, so the model
            // must answer in prose rather than asking for something it cannot
            // be given -- otherwise the turn ends with nothing to show. Zero
            // means unbounded, so that round never arrives.
            let exhausted = max_rounds > 0 && round + 1 >= max_rounds;

            let completion = host::chat(&CompletionRequest {
                messages: messages.clone(),
                tools: if exhausted { Vec::new() } else { tools() },
                model: None,
                temperature: None,
                max_tokens: None,
            })?;

            // A model that narrates before calling a tool has already been
            // seen saying it, so the separator has to reach the browser too --
            // through `progress`, which feeds the same stream the reply does.
            if !completion.content.is_empty() {
                if !reply.is_empty() {
                    host::progress("\n\n");
                    reply.push_str("\n\n");
                }
                reply.push_str(&completion.content);
            }

            if completion.tool_calls.is_empty() {
                // The turn would end here. Anything the user has said since it
                // began extends it instead of being answered separately --
                // which is what makes a follow-up feel like part of the same
                // exchange rather than a new one.
                let waiting = host::pending_input();
                if !waiting.is_empty() {
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: completion.content,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    });
                    messages.extend(injected(&waiting));
                    round += 1;
                    continue;
                }
                return Ok(reply);
            }

            // A "length" finish means the output was cut off at the token
            // limit, so every tool call in this message may carry arguments
            // truncated mid-JSON. Some will still parse -- into something the
            // model never meant -- so none of them are run. The model is told
            // instead, and can ask again more briefly.
            if completion.finish_reason.as_deref() == Some("length") {
                host::log("warn", "reply was truncated; refusing its tool calls");
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: completion.content,
                    tool_calls: completion.tool_calls.clone(),
                    tool_call_id: None,
                });
                for call in &completion.tool_calls {
                    messages.push(Message {
                        role: "tool".to_string(),
                        content:
                            r#"{"error":"not run: the message was cut off at the token limit and these arguments may be incomplete"}"#
                                .to_string(),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(call.id.clone()),
                    });
                }
                round += 1;
                continue;
            }

            host::log(
                "info",
                &format!("running {} tool call(s)", completion.tool_calls.len()),
            );

            // Announced before running, so the browser shows what is happening
            // while it happens rather than explaining it afterwards.
            let mut echoed = Vec::with_capacity(completion.tool_calls.len());
            for call in &completion.tool_calls {
                let (action, without_action) = split_action(&call.arguments);
                host::tool_started(&ToolActivity {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    action,
                });
                echoed.push(ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: without_action,
                });
            }

            // The model's own request has to go back into the conversation
            // before its answers do, or the results refer to a call the model
            // cannot see. It goes back without the label: that was written
            // for the user, and replaying it would pay for those tokens on
            // every subsequent turn.
            messages.push(Message {
                role: "assistant".to_string(),
                content: completion.content,
                tool_calls: echoed,
                tool_call_id: None,
            });
            for call in &completion.tool_calls {
                messages.push(run_tool(call));
            }

            // Injected after the results and before the next model call: the
            // only point in the loop where the conversation is consistent and
            // nothing is half-done.
            messages.extend(injected(&host::pending_input()));

            round += 1;
            if exhausted {
                // The round that withheld tools still asked for them, which
                // means the model ignored their absence. Nothing further can
                // be offered, so the turn ends with whatever was said.
                return Ok(reply);
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);
