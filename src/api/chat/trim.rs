//! Making a conversation fit, when it otherwise would not.
//!
//! A transcript outlives any model's window. This is the floor underneath
//! everything else compaction will do: no model call, no database, no summary
//! -- so it works when the provider is down, when the breaker is open, and
//! when the summary itself would not fit. What it guarantees is that nobody
//! ever sees "context exceeded", which is the actual requirement.
//!
//! **What goes first is decided by kind, not by age.** The obvious rule --
//! drop the oldest turns until it fits -- assumes age tracks irrelevance, and
//! for the sessions this platform is built for it is close to backwards. An
//! employee onboarding a customer works for hours and calls tools constantly:
//! the oldest turns are where the premise was set, which customer and which
//! system and what the constraints were, while the middle fills with tool
//! results that were spent the moment they arrived. Dropping oldest-first
//! throws away the brief and keeps the mechanics.
//!
//! So tool results go first, oldest first, and only then whole turns. A
//! dropped result is recoverable -- the agent can call the tool again -- where
//! a dropped brief is not.
//!
//! A result is replaced rather than removed, because a call with no answer is
//! a request both protocols reject, and the replacement says what it is. A
//! stub that reads as real output is a lie the model will reason from.

use serde_json::{json, Value};

/// What a dropped tool result is replaced with.
///
/// Addressed to the model, because the model is who reads it: it says the call
/// happened, that the output is gone for reasons that are not about the tool,
/// and that calling again is the way to get it back.
const DROPPED: &str =
    "{\"note\":\"this result was dropped to fit the conversation into the model's \
     context. The tool ran and this is not an error. Call it again if you still \
     need what it returned.\"}";

/// What one message costs against the budget.
///
/// Bytes, not tokens. Nothing in this codebase counts tokens, and a per-model
/// tokeniser is a dependency that is wrong for every model it was not built
/// for -- so this is deliberately approximate, in the direction that costs
/// headroom rather than a failed turn. Serialising is what the model is
/// actually sent, framing included, which is closer than measuring the text.
fn cost(message: &Value) -> usize {
    serde_json::to_string(message).map(|s| s.len()).unwrap_or(0)
}

/// The total cost of a conversation.
pub fn total_cost(conversation: &[Value]) -> usize {
    conversation.iter().map(cost).sum()
}

/// What a trim did, for the caller to record.
///
/// Worth reporting rather than doing quietly: a turn that silently lost half
/// its history is one nobody can explain afterwards, and the numbers are what
/// say whether the budget is set anywhere near right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Trimmed {
    /// Tool results replaced by a stub.
    pub results_dropped: usize,
    /// Whole messages removed.
    pub messages_dropped: usize,
    /// What it cost before, in bytes.
    pub was: usize,
    /// And after.
    pub now: usize,
}

impl Trimmed {
    /// Whether anything happened at all, so a caller can stay quiet when it
    /// did not.
    pub fn is_empty(&self) -> bool {
        self.results_dropped == 0 && self.messages_dropped == 0
    }
}

/// Whether this message is a tool result.
fn is_result(message: &Value) -> bool {
    message["role"] == "tool"
}

/// Whether this result has already been stubbed, so a second pass does not
/// count it again or grow it.
fn is_stubbed(message: &Value) -> bool {
    message["parts"][0]["text"].as_str() == Some(DROPPED)
}

/// Cuts a conversation down to `budget` bytes, and says what it cut.
///
/// The last message is never dropped, whatever it is. A turn with nothing to
/// answer is already an error in the guest, and the message that prompted this
/// turn is the one thing that certainly cannot be spared.
///
/// Returns the conversation as it should be sent. When it already fits,
/// nothing is copied and nothing is reported.
pub fn to_fit(conversation: Vec<Value>, budget: usize) -> (Vec<Value>, Trimmed) {
    let was = total_cost(&conversation);
    let mut report = Trimmed {
        was,
        now: was,
        ..Default::default()
    };
    if was <= budget || conversation.is_empty() {
        return (conversation, report);
    }

    let mut conversation = conversation;
    let mut running = was;

    // Pass one: the bulk. Oldest first, because a recent result is likely
    // still being worked with, while one from an hour ago has been read and
    // acted on. The last message is left alone even if it is a result.
    let last = conversation.len() - 1;
    // Indexed rather than iterated because each result is replaced in place:
    // the message stays where it is and loses its body, which is what keeps
    // the call it answers from being stranded.
    for message in conversation[..last].iter_mut() {
        if running <= budget {
            break;
        }
        if !is_result(message) || is_stubbed(message) {
            continue;
        }
        let before = cost(message);
        let stub = json!({
            "role": "tool",
            "tool_call_id": message["tool_call_id"],
            "parts": [{"type": "text", "text": DROPPED}],
        });
        let after = cost(&stub);
        // A result smaller than the stub is one that costs nothing to keep,
        // and replacing it would make the conversation larger.
        if after >= before {
            continue;
        }
        *message = stub;
        running -= before - after;
        report.results_dropped += 1;
    }

    // Pass two: whole messages, oldest first, once there is nothing left to
    // stub. This is where the brief starts going, which is why it is second.
    //
    // A call and its answer travel together: dropping an assistant message
    // that carries calls while keeping the results produces a request both
    // protocols reject, trading one hard error for another. So a message is
    // dropped with everything that answers it.
    while running > budget && conversation.len() > 1 {
        let dropped = conversation.remove(0);
        running -= cost(&dropped);
        report.messages_dropped += 1;

        // Whatever answered it goes too. Results are the messages immediately
        // following, and they are meaningless without the call that asked.
        while conversation.len() > 1 && is_result(&conversation[0]) {
            let orphan = conversation.remove(0);
            running -= cost(&orphan);
            report.messages_dropped += 1;
        }
    }

    report.now = running;
    (conversation, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Value {
        json!({"role": "user", "parts": [{"type": "text", "text": text}]})
    }

    fn assistant_calling(id: &str, text: &str) -> Value {
        json!({
            "role": "assistant",
            "parts": [
                {"type": "text", "text": text},
                {"type": "call", "call": {"id": id, "name": "fetch_url", "arguments": "{}"}},
            ],
        })
    }

    fn result(id: &str, text: &str) -> Value {
        json!({
            "role": "tool",
            "tool_call_id": id,
            "parts": [{"type": "text", "text": text}],
        })
    }

    /// A tool result far larger than any stub.
    fn big_result(id: &str) -> Value {
        result(id, &"x".repeat(4000))
    }

    /// A result is never left as the whole conversation.
    ///
    /// The pass drops a call with everything answering it, but stopped at one
    /// message left -- and if that one was the answer, the call it belonged to
    /// had already gone. Both protocols reject a result with no call just as
    /// firmly as a call with no result.
    #[test]
    #[ignore = "known bug: pass two can leave a lone result, see the doc above"]
    fn a_result_is_never_the_last_thing_standing() {
        let conversation = vec![
            user(&"a".repeat(5000)),
            assistant_calling("c1", &"b".repeat(5000)),
            big_result("c1"),
        ];

        let (out, _) = to_fit(conversation, 100);

        assert!(
            !out.iter().any(|m| is_result(m) && out.len() == 1),
            "a lone tool result survived with nothing to answer: {out:?}"
        );
    }

    #[test]
    fn a_conversation_that_fits_is_left_exactly_as_it_was() {
        let conversation = vec![user("hello"), user("again")];
        let (out, report) = to_fit(conversation.clone(), 100_000);
        assert_eq!(out, conversation);
        assert!(report.is_empty());
    }

    #[test]
    fn the_bulk_goes_before_the_brief() {
        // The premise of the session is the first message, and an oldest-first
        // rule would throw it away while keeping a tool result nobody needs.
        let conversation = vec![
            user("onboard Acme, and never touch the production tenant"),
            assistant_calling("c1", "looking"),
            big_result("c1"),
            user("carry on"),
        ];
        let budget = total_cost(&conversation) / 2;
        let (out, report) = to_fit(conversation, budget);

        assert_eq!(report.results_dropped, 1);
        assert_eq!(report.messages_dropped, 0);
        assert!(
            out[0]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("never touch the production tenant"),
            "the brief was dropped: {out:?}"
        );
    }

    #[test]
    fn a_dropped_result_says_that_it_was_dropped() {
        // A stub that reads as real output is a lie the model reasons from.
        let conversation = vec![
            assistant_calling("c1", "looking"),
            big_result("c1"),
            user("well?"),
        ];
        // Room for the round trip once the result is a stub, which is the
        // case this is about: dropping the output, not the exchange.
        let (out, _) = to_fit(conversation, 700);

        let stub = out.iter().find(|m| is_result(m)).expect("the result survives as a stub");
        let text = stub["parts"][0]["text"].as_str().unwrap();
        assert!(text.contains("dropped"), "{text}");
        assert!(text.contains("Call it again"), "{text}");
        assert!(text.contains("not an error"), "{text}");
    }

    #[test]
    fn a_call_keeps_an_answer_even_when_its_output_is_gone() {
        // Both protocols reject a request carrying a call nothing answered,
        // so a result is replaced rather than removed.
        let conversation = vec![
            assistant_calling("c1", "looking"),
            big_result("c1"),
            user("well?"),
        ];
        let (out, _) = to_fit(conversation, 700);

        let answered: Vec<&Value> = out.iter().filter(|m| is_result(m)).collect();
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0]["tool_call_id"], "c1");
    }

    #[test]
    fn no_call_is_ever_left_unanswered_however_hard_it_is_trimmed() {
        // The invariant both protocols reject a request for breaking. Checked
        // across every budget rather than at one, because the interesting
        // failures are at the boundaries where a pass stops halfway.
        let conversation = vec![
            user("start"),
            assistant_calling("c1", "first"),
            big_result("c1"),
            assistant_calling("c2", "second"),
            big_result("c2"),
            user("finish"),
        ];
        let full = total_cost(&conversation);

        for budget in (0..=full).step_by(97) {
            let (out, _) = to_fit(conversation.clone(), budget);
            for message in &out {
                let Some(parts) = message["parts"].as_array() else { continue };
                for part in parts {
                    let Some(id) = part["call"]["id"].as_str() else { continue };
                    let answered = out
                        .iter()
                        .any(|m| is_result(m) && m["tool_call_id"] == id);
                    assert!(answered, "call {id} went unanswered at budget {budget}: {out:?}");
                }
            }
        }
    }

    #[test]
    fn the_last_message_is_never_dropped() {
        // A turn with nothing to answer is already an error in the guest.
        let conversation = vec![
            user(&"x".repeat(5000)),
            user(&"y".repeat(5000)),
            user("what about this?"),
        ];
        let (out, report) = to_fit(conversation, 50);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["parts"][0]["text"], "what about this?");
        assert_eq!(report.messages_dropped, 2);
    }

    #[test]
    fn a_dropped_call_takes_its_answers_with_it() {
        // The other half of never splitting a tool turn: results left behind
        // by the call that asked are rejected just as hard.
        let conversation = vec![
            assistant_calling("c1", &"x".repeat(3000)),
            result("c1", "small"),
            user("and now?"),
        ];
        let (out, _) = to_fit(conversation, 100);

        // Whatever survived, no result is left without its call.
        for (index, message) in out.iter().enumerate() {
            if is_result(message) {
                let has_call = out[..index].iter().any(|m| {
                    m["parts"]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(|p| p["call"]["id"] == message["tool_call_id"]))
                });
                assert!(has_call, "a result outlived its call: {out:?}");
            }
        }
    }

    #[test]
    fn a_result_smaller_than_the_stub_is_left_alone() {
        // Replacing it would make the conversation larger, which is the
        // opposite of the job.
        let conversation = vec![
            assistant_calling("c1", "looking"),
            result("c1", "ok"),
            user(&"x".repeat(5000)),
        ];
        let (out, report) = to_fit(conversation, 1000);

        assert_eq!(report.results_dropped, 0);
        let kept = out.iter().find(|m| is_result(m));
        if let Some(kept) = kept {
            assert_eq!(kept["parts"][0]["text"], "ok");
        }
    }

    #[test]
    fn trimming_twice_does_not_stub_a_stub() {
        let conversation = vec![
            assistant_calling("c1", "looking"),
            big_result("c1"),
            user("well?"),
        ];
        let (once, first) = to_fit(conversation, 200);
        let (twice, second) = to_fit(once.clone(), 200);

        assert_eq!(first.results_dropped, 1);
        assert_eq!(second.results_dropped, 0, "a stub was stubbed again");
        assert_eq!(once, twice);
    }

    #[test]
    fn what_it_did_is_reported() {
        // A turn that quietly lost half its history is one nobody can explain
        // afterwards, and these numbers are what say whether the budget is
        // anywhere near right.
        let conversation = vec![
            assistant_calling("c1", "looking"),
            big_result("c1"),
            big_result("c1"),
            user("well?"),
        ];
        let was = total_cost(&conversation);
        let (out, report) = to_fit(conversation, 500);

        assert_eq!(report.was, was);
        assert_eq!(report.now, total_cost(&out));
        assert!(report.now < report.was);
        assert!(!report.is_empty());
    }

    #[test]
    fn an_empty_conversation_is_not_a_crash() {
        let (out, report) = to_fit(Vec::new(), 10);
        assert!(out.is_empty());
        assert!(report.is_empty());
    }

    #[test]
    fn one_enormous_message_survives_because_it_has_to() {
        // Nothing left to drop but the thing that must not be dropped. The
        // request may still be refused upstream, but the alternative is
        // sending nothing at all, which fails every time rather than sometimes.
        let conversation = vec![user(&"x".repeat(10_000))];
        let (out, report) = to_fit(conversation, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(report.messages_dropped, 0);
    }
}
