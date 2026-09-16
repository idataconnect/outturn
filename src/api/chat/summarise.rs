//! Standing in for the part of a conversation that no longer fits.
//!
//! The trim underneath this drops tool results and then whole turns, which is
//! a guarantee rather than a good outcome: what it loses, it loses. A summary
//! is the attempt to lose less -- the early turns become a paragraph, and the
//! paragraph is what a later turn reads instead.
//!
//! **What goes in is bounded by construction.** The system prompt, the
//! previous summary, and the tail. Never the whole transcript, because the
//! whole transcript is the thing that does not fit -- and because the window
//! belongs to the route rather than the session, so a turn can arrive at a
//! smaller context than the one before it. Summarising from a bounded input
//! means the incoming model can always do it, however long the session has
//! run, and no compaction depends on a model that is being switched away
//! from.
//!
//! That rules out the obvious ordering -- compact in the outgoing model before
//! switching -- which also bills somebody for an expensive operation at the
//! moment they asked for something else.
//!
//! **Summaries are cumulative.** Each one summarises the tail plus the summary
//! before it, because the alternative loses durable facts: a constraint stated
//! once at the start is exactly what gets dropped and then violated. Carrying
//! it forward is not a guarantee, only a much better chance. The guarantee is
//! what compaction carry-over is for, and that is not built.
//!
//! **A summary is marked as one.** It is a model's account of a conversation
//! that will be replayed on every later turn, so its failure mode is quiet: a
//! summary that misstates a decision becomes the record. Marking it means a
//! reader can see what happened, and the next summary knows it is summarising
//! a summary.

use serde_json::Value;

/// The traffic type a summary is asked for under.
///
/// Its own class, like naming, so a deployment can send it somewhere cheaper
/// than wherever conversations go. Summarising is background work the user did
/// not ask for, and paying frontier prices for it is a choice rather than a
/// requirement.
pub const TRAFFIC_TYPE: &str = "compaction";

/// Marks a message as a summary, and says what it stands in for.
///
/// In metadata rather than in a new role: the schema allows four roles and a
/// summary is an assistant message whatever else it is. `summary_through` is
/// the id of the last message it covers -- everything at or below that id is
/// replaced by this one when a conversation is projected.
pub const SUMMARY_MARK: &str = "summary_through";

/// How much of the conversation is left verbatim behind a summary.
///
/// The tail is where the live context is: what is being worked on now, the
/// recent tool results, the thread of what is being said. Summarising it would
/// be summarising the present, so the tail is what the summary is written
/// *from* and what survives it intact.
pub const TAIL_MESSAGES: usize = 10;

/// What a summary is asked to produce.
///
/// Written for the model that reads it back rather than for a person: the next
/// turn sees this in place of an hour of conversation, and what it needs is
/// what was decided and what must not be forgotten, not a narrative of who
/// said what.
pub const INSTRUCTION: &str = "\
You are summarising the earlier part of a conversation so it can be replaced by \
your summary. What you write will be given to the assistant in place of those \
messages, and it will not be able to see them again.

Record, as compactly as you can:
- What the user is trying to achieve, and any constraint they stated. Constraints \
  stated once are exactly what gets lost, so carry every one forward verbatim.
- Decisions made and their reasons.
- Facts established by tool calls that later work depends on: identifiers, names, \
  paths, values.
- Anything left unfinished.

Do not write a narrative and do not editorialise. Do not invent anything you \
cannot see. If an earlier summary is included, carry its content forward as well \
as the new messages -- it is the only record of what came before it.";

/// Where a summary would cut, given a conversation.
///
/// Returns the index the tail begins at, or `None` when there is nothing worth
/// summarising -- a conversation shorter than the tail it would keep has
/// nothing behind that tail to replace.
pub fn boundary(messages: usize) -> Option<usize> {
    // Strictly greater: a conversation exactly the length of the tail would
    // summarise nothing, and asking a model for a summary of nothing spends a
    // call to produce a paragraph saying so.
    (messages > TAIL_MESSAGES + 1).then(|| messages - TAIL_MESSAGES)
}

/// Builds the request sent to the model.
///
/// The system prompt leads, because a summary written without it is a summary
/// of what happened rather than of what matters. Then whatever is being
/// replaced, and the instruction last -- a model reads the final instruction as
/// the one that stands, and what is being asked for should not be buried under
/// the thing it is being asked about.
pub fn request(system_prompt: &str, replacing: &[Value]) -> Vec<Value> {
    let mut messages = Vec::with_capacity(replacing.len() + 2);
    if !system_prompt.trim().is_empty() {
        messages.push(serde_json::json!({
            "role": "system",
            "content": format!(
                "The assistant in the conversation below was given these \
                 instructions:\n\n{system_prompt}"
            ),
        }));
    }

    for message in replacing {
        // Flattened to text: a summariser needs what was said, and replaying
        // tool calls to it would mean answering them, which is a conversation
        // rather than a summary.
        let role = message["role"].as_str().unwrap_or("user");
        let text = flatten(message);
        if text.trim().is_empty() {
            continue;
        }
        messages.push(serde_json::json!({
            "role": if role == "tool" { "user" } else { role },
            "content": if role == "tool" {
                format!("[result of a tool call]\n{text}")
            } else {
                text
            },
        }));
    }

    messages.push(serde_json::json!({"role": "user", "content": INSTRUCTION}));
    messages
}

/// Everything a projected message says, as text.
fn flatten(message: &Value) -> String {
    let Some(parts) = message["parts"].as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text") => {
                if let Some(text) = part["text"].as_str() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            Some("call") => {
                // Named rather than reproduced: that a tool was called is
                // often the fact worth keeping, while its arguments rarely
                // are.
                if let Some(name) = part["call"]["name"].as_str() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&format!("[called {name}]"));
                }
            }
            _ => {}
        }
    }
    out
}

/// How a summary enters a conversation being projected.
///
/// Everything it covers is dropped and the summary stands where they were, so
/// what the model sees is the paragraph and then the tail. The stored
/// transcript is untouched -- a reader still has every message, which is the
/// point of replacing only the projection.
pub fn apply(conversation: Vec<Value>, summary: &str, through: usize) -> Vec<Value> {
    let mut out = Vec::with_capacity(conversation.len() - through + 1);
    out.push(serde_json::json!({
        "role": "assistant",
        "parts": [{
            "type": "text",
            "text": format!(
                "[summary of the earlier part of this conversation, which is no \
                 longer visible]\n\n{summary}"
            ),
        }],
    }));
    out.extend(conversation.into_iter().skip(through));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> Value {
        json!({"role": "user", "parts": [{"type": "text", "text": text}]})
    }

    fn calling(name: &str) -> Value {
        json!({
            "role": "assistant",
            "parts": [
                {"type": "text", "text": "looking"},
                {"type": "call", "call": {"id": "c1", "name": name, "arguments": "{}"}},
            ],
        })
    }

    fn result(text: &str) -> Value {
        json!({"role": "tool", "tool_call_id": "c1", "parts": [{"type": "text", "text": text}]})
    }

    #[test]
    fn a_short_conversation_is_not_worth_summarising() {
        // Asking a model to summarise nothing spends a call to be told so.
        assert_eq!(boundary(0), None);
        assert_eq!(boundary(TAIL_MESSAGES), None);
        assert_eq!(boundary(TAIL_MESSAGES + 1), None);
    }

    #[test]
    fn the_tail_is_what_survives_intact() {
        // The live context -- what is being worked on now -- is not something
        // to summarise, so the cut leaves exactly the tail behind it.
        let cut = boundary(TAIL_MESSAGES + 5).expect("worth summarising");
        assert_eq!(cut, 5);
        assert_eq!(TAIL_MESSAGES + 5 - cut, TAIL_MESSAGES);
    }

    #[test]
    fn the_system_prompt_leads_the_thing_being_summarised() {
        // A summary written without it is a summary of what happened rather
        // than of what matters.
        let out = request("never touch production", &[user("hello")]);
        assert_eq!(out[0]["role"], "system");
        assert!(
            out[0]["content"].as_str().unwrap().contains("never touch production"),
            "{out:?}"
        );
    }

    #[test]
    fn the_instruction_comes_last() {
        // A model reads the final instruction as the one that stands, and what
        // is being asked for should not be buried under what it is about.
        let out = request("sys", &[user("a"), user("b")]);
        let last = out.last().unwrap();
        assert_eq!(last["role"], "user");
        assert!(last["content"].as_str().unwrap().contains("summarising"));
    }

    #[test]
    fn a_tool_result_is_offered_as_something_that_happened() {
        // Not as a tool message: replaying one would mean the summariser had
        // been asked a question it has to answer, which is a conversation
        // rather than a summary.
        let out = request("", &[calling("fetch_url"), result("200 OK")]);
        assert!(out.iter().all(|m| m["role"] != "tool"), "{out:?}");
        let mentioned = out
            .iter()
            .any(|m| m["content"].as_str().is_some_and(|c| c.contains("result of a tool call")));
        assert!(mentioned, "{out:?}");
    }

    #[test]
    fn a_call_is_named_rather_than_reproduced() {
        // That a tool was called is often what matters; its arguments rarely
        // are, and they are bulk.
        let out = request("", &[calling("expand_archive")]);
        let text = out[0]["content"].as_str().unwrap();
        assert!(text.contains("[called expand_archive]"), "{text}");
    }

    #[test]
    fn an_empty_message_is_not_sent() {
        // A placeholder reply that never got its text would otherwise arrive
        // as an empty turn, which some providers reject outright.
        let out = request("", &[json!({"role": "assistant", "parts": []}), user("hi")]);
        assert_eq!(out.len(), 2, "{out:?}");
    }

    #[test]
    fn the_summary_stands_where_the_messages_were() {
        let conversation = vec![user("one"), user("two"), user("three"), user("four")];
        let out = apply(conversation, "they discussed numbers", 2);

        assert_eq!(out.len(), 3);
        assert!(out[0]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("they discussed numbers"));
        assert_eq!(out[1]["parts"][0]["text"], "three");
        assert_eq!(out[2]["parts"][0]["text"], "four");
    }

    #[test]
    fn a_summary_says_that_it_is_one() {
        // It will be replayed on every later turn, and a model reading it as
        // ordinary conversation would treat a paraphrase as something somebody
        // actually said.
        let out = apply(vec![user("a"), user("b")], "the gist", 1);
        let text = out[0]["parts"][0]["text"].as_str().unwrap();
        assert!(text.contains("summary"), "{text}");
        assert!(text.contains("no longer visible"), "{text}");
    }

    #[test]
    fn a_summary_of_a_summary_carries_the_older_one_forward() {
        // The instruction has to say so: the previous summary is the only
        // record of what came before it, and a model told to summarise
        // "messages" may treat it as one more message to compress away.
        assert!(INSTRUCTION.contains("earlier summary"), "{INSTRUCTION}");
        assert!(INSTRUCTION.contains("only record"), "{INSTRUCTION}");
    }

    #[test]
    fn the_instruction_forbids_inventing() {
        // A summary that misstates a decision becomes the record, and nothing
        // downstream can tell it from what was said.
        assert!(INSTRUCTION.contains("Do not invent"), "{INSTRUCTION}");
    }
}
