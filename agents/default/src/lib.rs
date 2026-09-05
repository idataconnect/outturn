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
    self, Arrival, CompletionRequest, Message, ToolActivity, ToolCall, ToolDefinition, ToolOutcome,
};

struct Component;

/// The model's name for the clock tool.
const CURRENT_TIME: &str = "get_current_time";
const READ_OBJECT: &str = "read_object";
const WRITE_OBJECT: &str = "write_object";
const LIST_OBJECTS: &str = "list_objects";

/// The model's name for the outbound request tool.
const FETCH: &str = "fetch_url";

/// How much of a file a single read puts in front of the model.
///
/// Comfortably under the generic tool ceiling, so an assembled head-and-tail
/// is not then cut in half again by the truncator that follows it.
const READ_BUDGET: u64 = 32 * 1024;

/// How that budget splits when both ends are worth having.
///
/// Head-heavy because a file's beginning usually establishes what it is --
/// headers, imports, structure -- while its end is only sometimes the
/// interesting part. Where the end is what matters, as in a log, the model
/// asks for it directly rather than relying on this number, which is why the
/// ratio can afford to be a guess.
const HEAD_SHARE: u64 = 70;

/// The argument every tool carries so the user can see what is happening.
///
/// Asked of the model rather than inferred: only the model knows what it is
/// about to do, and a label written by the guest would be a plausible fiction
/// attributed to the agent.
const ACTION: &str = "action";

fn tools() -> Vec<ToolDefinition> {
    vec![
    ToolDefinition {
        name: READ_OBJECT.to_string(),
        description: "Read a stored file. Paths are relative to this agent's \
                      own storage -- there is nothing above it to reach, so \
                      do not try. A file too large to show comes back as its \
                      start and its end, saying how much was skipped and at \
                      what offset the rest begins."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"path":{"type":"string","description":"Relative path, e.g. reports/q3.csv"},"offset":{"type":"integer","description":"Byte to start from. Omit for the beginning.","default":0},"from_end":{"type":"boolean","description":"Read the end of the file instead of the start. Use this for logs, where what went wrong is at the bottom.","default":false},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Reading last quarter's figures."}},"required":["path","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: WRITE_OBJECT.to_string(),
        description: "Write a file to this agent's storage, replacing whatever \
                      was there. Paths are relative to its own space."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"path":{"type":"string","description":"Relative path, e.g. reports/summary.md"},"content":{"type":"string","description":"The complete new contents."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Saving the summary."}},"required":["path","content","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: LIST_OBJECTS.to_string(),
        description: "List stored files. Omit the prefix to see everything."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"prefix":{"type":"string","description":"Relative prefix, e.g. reports/. Omit for everything."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Looking through the stored files."}},"required":["action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: FETCH.to_string(),
        description: "Make an HTTP request to an external API. Only hosts this \
                      workspace has allowed can be reached; anything else comes \
                      back refused, and no amount of rephrasing changes that -- \
                      say so rather than trying another address. Credentials \
                      are attached automatically where they are configured, so \
                      never put a key in the URL or in a header. Redirects are \
                      not followed: a 3xx response means asking again for the \
                      new location."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"url":{"type":"string","description":"Absolute https URL."},"method":{"type":"string","description":"GET, POST, PUT, PATCH, DELETE or HEAD. Defaults to GET.","enum":["GET","POST","PUT","PATCH","DELETE","HEAD"]},"headers":{"type":"object","description":"Extra headers, as a flat object. Leave authorization out; it is added for you.","additionalProperties":{"type":"string"}},"body":{"type":"string","description":"Request body, for methods that take one."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Looking up the exchange rate."}},"required":["url","action"]}"#
            .to_string(),
    },
    ToolDefinition {
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
    },
    ]
}

/// Reads a JSON string argument, or an empty string if it is missing.
fn arg<'a>(args: &'a serde_json::Value, name: &str) -> &'a str {
    args.get(name).and_then(|v| v.as_str()).unwrap_or("")
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

/// Two independent ceilings on what a tool may put in front of the model.
///
/// Whichever is reached first wins, because they fail differently: a single
/// enormous line passes any line count, and a hundred thousand short ones pass
/// any byte count.
const MAX_TOOL_LINES: usize = 2000;
const MAX_TOOL_BYTES: usize = 50 * 1024;

/// Cuts a result down to what the model should see.
///
/// Never mid-line: half a line of JSON or code reads as though it were whole,
/// and a model will act on it. What was removed is described rather than
/// silently dropped, so the model can ask for it differently instead of
/// concluding the file was short.
fn for_the_model(full: &str) -> String {
    let total_lines = full.lines().count();
    let total_bytes = full.len();
    if total_lines <= MAX_TOOL_LINES && total_bytes <= MAX_TOOL_BYTES {
        return full.to_string();
    }

    let mut kept = String::new();
    let mut lines = 0usize;
    for line in full.lines() {
        if lines >= MAX_TOOL_LINES || kept.len() + line.len() + 1 > MAX_TOOL_BYTES {
            break;
        }
        kept.push_str(line);
        kept.push('\n');
        lines += 1;
    }

    format!(
        "{kept}\n[truncated: showing {lines} of {total_lines} lines, \
         {} of {total_bytes} bytes]",
        kept.len()
    )
}

/// Reads a range and turns the bytes into something a model can use.
///
/// Not text is reported as not text, rather than mangled into replacement
/// characters: a model told "42KB of binary" can decide what to do, where a
/// model shown mojibake will try to read it.
fn slice(path: &str, offset: u64, len: u64) -> Result<String, String> {
    let bytes = host::read_object(path, offset, len.min(u32::MAX as u64) as u32)?;
    String::from_utf8(bytes).map_err(|_| "this file is not text and cannot be shown".to_string())
}

/// Trims a fragment back to whole lines.
///
/// A range lands wherever the byte count says, which is usually mid-line. Half
/// a line reads as though it were whole and a model will act on it, so the
/// partial line at the cut is dropped -- from the end of a head, from the
/// start of a tail.
fn whole_lines(fragment: &str, drop_from_start: bool) -> &str {
    if drop_from_start {
        match fragment.find('\n') {
            Some(at) => &fragment[at + 1..],
            None => fragment,
        }
    } else {
        match fragment.rfind('\n') {
            Some(at) => &fragment[..at],
            None => fragment,
        }
    }
}

/// Reads a file, showing both ends when it will not fit.
///
/// Head-only truncation loses exactly the part that matters in a log or a
/// stack trace, where the setup is at the top and the failure at the bottom.
/// Both ends are shown instead, and what was skipped is described precisely
/// enough to go and get: a model that wants the middle can ask for it by
/// offset rather than guessing.
fn read_object(args: &serde_json::Value) -> String {
    let path = arg(args, "path");
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
    let from_end = args.get("from_end").and_then(|v| v.as_bool()).unwrap_or(false);

    let size = match host::stat_object(path) {
        Ok(info) => info.size,
        Err(e) => return serde_json::json!({ "path": path, "error": e }).to_string(),
    };

    // The end, asked for directly. Possible only because the size is known
    // here -- a caller cannot name an offset it would have to have measured.
    if from_end {
        let start = size.saturating_sub(READ_BUDGET);
        return match slice(path, start, READ_BUDGET) {
            Ok(text) => {
                let shown = if start > 0 { whole_lines(&text, true) } else { &text[..] };
                serde_json::json!({
                    "path": path, "size": size, "showing": "end",
                    "content": if start > 0 {
                        format!("[{start} earlier bytes not shown]\n{shown}")
                    } else {
                        shown.to_string()
                    },
                })
                .to_string()
            }
            Err(e) => serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
        };
    }

    let remaining = size.saturating_sub(offset);
    if remaining <= READ_BUDGET {
        return match slice(path, offset, remaining.max(1)) {
            Ok(text) => serde_json::json!({
                "path": path, "size": size, "offset": offset,
                "showing": "all", "content": text,
            })
            .to_string(),
            Err(e) => serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
        };
    }

    let head_len = READ_BUDGET * HEAD_SHARE / 100;
    let tail_len = READ_BUDGET - head_len;
    let tail_start = size - tail_len;

    let head = match slice(path, offset, head_len) {
        Ok(text) => text,
        Err(e) => return serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
    };
    let tail = match slice(path, tail_start, tail_len) {
        Ok(text) => text,
        Err(e) => return serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
    };

    let head = whole_lines(&head, false);
    let tail = whole_lines(&tail, true);
    let skipped_from = offset + head.len() as u64;
    let skipped = tail_start.saturating_sub(skipped_from);

    serde_json::json!({
        "path": path,
        "size": size,
        "showing": "start and end",
        // Said precisely enough to act on: a model wanting the middle reads
        // again from this offset rather than guessing where it went.
        "content": format!(
            "{head}\n[{skipped} bytes not shown; read again with offset {skipped_from} \
             for the middle]\n{tail}"
        ),
    })
    .to_string()
}

fn write_object(args: &serde_json::Value) -> String {
    let path = arg(args, "path");
    match host::write_object(path, arg(args, "content").as_bytes()) {
        Ok(written) => serde_json::json!({ "path": path, "bytes": written }).to_string(),
        Err(e) => serde_json::json!({ "path": path, "error": e }).to_string(),
    }
}

fn list_objects(args: &serde_json::Value) -> String {
    match host::list_objects(arg(args, "prefix")) {
        Ok(found) => serde_json::json!({
            "files": found
                .iter()
                .map(|f| serde_json::json!({ "path": f.path, "size": f.size }))
                .collect::<Vec<_>>(),
        })
        .to_string(),
        Err(e) => serde_json::json!({ "error": e }).to_string(),
    }
}

/// Asks the host to make a request.
///
/// Everything that decides whether this is allowed happens on the other side
/// of the boundary. What is left here is turning the model's arguments into a
/// request and its answer into something worth reading: a refusal says why in
/// a sentence, because a tool that fails opaquely gets called again the same
/// way, and a body that was cut short says so rather than trailing off.
fn fetch_url(args: &serde_json::Value) -> String {
    let url = arg(args, "url");
    if url.is_empty() {
        return serde_json::json!({ "error": "no url was given" }).to_string();
    }

    let method = match arg(args, "method") {
        "" => "GET".to_string(),
        given => given.to_string(),
    };

    // A flat object, because that is what a model reliably produces. Anything
    // else is dropped rather than guessed at.
    let headers: Vec<(String, String)> = args
        .get("headers")
        .and_then(|h| h.as_object())
        .map(|h| {
            h.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let body = args
        .get("body")
        .and_then(|b| b.as_str())
        .map(str::to_string);

    match host::fetch(&host::HttpRequest {
        method,
        url: url.to_string(),
        headers,
        body,
    }) {
        Ok(response) => serde_json::json!({
            "status": response.status,
            "body": response.body,
            "truncated": response.truncated,
            // The location, so a 3xx is actionable rather than a dead end --
            // the host will not follow one, and the model has to decide
            // whether the new address is worth asking for.
            "location": response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                .map(|(_, value)| value.clone()),
        })
        .to_string(),
        Err(e) => serde_json::json!({ "error": e }).to_string(),
    }
}

/// Runs one tool call and returns the message answering it.
fn run_tool(call: &ToolCall) -> Message {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);

    let content = match call.name.as_str() {
        READ_OBJECT => read_object(&args),
        WRITE_OBJECT => write_object(&args),
        LIST_OBJECTS => list_objects(&args),
        FETCH => fetch_url(&args),
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

    // The reader gets everything; the model gets what fits. Cutting it down
    // happens here and once: what the model is handed now is what it will be
    // shown on every later turn, so the two must be the same string.
    let is_error = content.contains("\"error\"");
    let for_model = for_the_model(&content);
    host::tool_finished(&ToolOutcome {
        id: call.id.clone(),
        details: content.clone(),
        content: for_model.clone(),
        is_error,
    });

    Message {
        role: "tool".to_string(),
        content: for_model,
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
                    arguments: without_action.clone(),
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
