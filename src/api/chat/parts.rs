//! What a reply is made of, and the one rule for assembling it.
//!
//! A stored reply is a sequence: the words the model wrote, the calls it made,
//! the points a message arrived mid-turn, and where it stopped to think. Three
//! places build that sequence -- the worker as a turn streams, `replay` when a
//! reload rebuilds one that is still being written, and the browser as events
//! arrive -- and they must produce the same shape from the same events or the
//! message changes under the reader when the turn ends.
//!
//! The two Rust ones share this. The browser's copy cannot, being in another
//! language, so `ui/src/lib/useChatRuntime.ts` restates the rule and this
//! comment is the pointer between them.

use serde::{Deserialize, Serialize};

/// One piece of a reply, in the order it happened.
///
/// An enum rather than loose JSON, so a kind added later is a compile error at
/// every place that reads one rather than a silent drop. The projection that
/// builds a model's request is the reason: its catch-all used to swallow
/// anything it did not recognise, and a part the model never sees is a failure
/// whose every symptom points somewhere else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Part {
    /// Words the model wrote.
    Text { text: String },
    /// A call it made, named by id. The call itself is in `tool_calls`, so
    /// nothing about it is written down twice.
    Call { id: String },
    /// Where a message the user sent mid-turn was handed over, naming it. The
    /// reply is drawn split here, since what follows is answering it.
    Steer { id: String },
    /// Where the model stopped to think, and for how long.
    ///
    /// Never sent back to a model: a model handed its own reasoning as a past
    /// utterance reads it as speech and answers it. `ms` is absent on a thought
    /// nothing timed -- zero is a measurement, and "0.0s" reads as a thought
    /// that took no time rather than one nobody clocked.
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<i64>,
    },
    /// A kind written by a different build. Kept rather than refused so a
    /// rolling deploy can read its own rows, and matched explicitly so nothing
    /// treats it as speech.
    #[serde(other)]
    Unknown,
}

/// Assembles a reply's parts, applying the one coalescing rule.
///
/// Fragments arrive a token at a time, and consecutive fragments of the same
/// kind are one part -- one utterance, one thought -- rather than dozens. The
/// rule is only ever "join to the last part when it is the same kind", and
/// having it in one place is what stops the live path and the reload path
/// drifting into rebuilding a message differently.
#[derive(Debug, Default)]
pub struct Builder {
    parts: Vec<Part>,
    /// When the thought now being written began, for the duration below. Only
    /// meaningful while the last part is a thought, which is the only time it
    /// is read.
    thinking_since: Option<chrono::DateTime<chrono::Utc>>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Words, joined to the words before them.
    pub fn text(&mut self, fragment: &str) {
        match self.parts.last_mut() {
            Some(Part::Text { text }) => text.push_str(fragment),
            _ => self.parts.push(Part::Text {
                text: fragment.to_string(),
            }),
        }
    }

    /// Thinking, joined to the thought in progress.
    ///
    /// `at` is when this fragment arrived, and the clock restarts on each new
    /// thought rather than running from the first: time spent waiting on a tool
    /// is not time the model spent thinking, and counting it reports a
    /// two-second thought as a thirty-second one.
    ///
    /// The caller supplies the clock because the two callers have different
    /// ones -- the worker measures wall time as fragments arrive, a reload
    /// measures from the timestamps the events were stored with -- and a thought
    /// nothing timed passes `None` and reports no duration.
    pub fn reasoning(&mut self, fragment: &str, at: Option<chrono::DateTime<chrono::Utc>>) {
        match self.parts.last_mut() {
            Some(Part::Reasoning { text, ms }) => {
                text.push_str(fragment);
                if let (Some(at), Some(began)) = (at, self.thinking_since) {
                    *ms = Some((at - began).num_milliseconds().max(0));
                }
            }
            _ => {
                self.thinking_since = at;
                self.parts.push(Part::Reasoning {
                    text: fragment.to_string(),
                    ms: None,
                });
            }
        }
    }

    /// A call, which never joins anything.
    pub fn call(&mut self, id: &str) {
        self.parts.push(Part::Call { id: id.to_string() });
    }

    /// A message handed over mid-turn, which never joins anything.
    pub fn steer(&mut self, id: &str) {
        self.parts.push(Part::Steer { id: id.to_string() });
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Whether anything here counts as the agent having spoken.
    ///
    /// Thinking counts. It is not an answer, but a reply holding a thought and
    /// nothing else is a turn the model spent deliberating -- the only account
    /// of where its tokens went -- and treating it as empty discards it and can
    /// wedge the session on the abandoned-placeholder guard.
    pub fn said_something(&self) -> bool {
        self.parts.iter().any(|p| match p {
            Part::Text { text } => !text.is_empty(),
            Part::Reasoning { .. } | Part::Call { .. } => true,
            Part::Steer { .. } | Part::Unknown => false,
        })
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    pub fn into_parts(self) -> Vec<Part> {
        self.parts
    }

    /// The parts as stored, which is JSON in a `jsonb` column.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!(self.parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0)
    }

    #[test]
    fn fragments_of_one_kind_become_one_part() {
        let mut b = Builder::new();
        b.text("Let me ");
        b.text("look.");
        assert_eq!(
            b.parts(),
            [Part::Text {
                text: "Let me look.".into()
            }]
        );
    }

    /// The order is the meaning: a thought before a call is about what to ask,
    /// and the one after it is about the answer.
    #[test]
    fn a_call_between_two_thoughts_keeps_them_apart() {
        let mut b = Builder::new();
        b.reasoning("what to ask", at(0));
        b.call("c1");
        b.reasoning("what came back", at(30));

        assert_eq!(b.parts().len(), 3);
        assert!(matches!(b.parts()[1], Part::Call { .. }));
    }

    /// Time waiting on a tool is not time spent thinking.
    #[test]
    fn the_clock_restarts_on_each_thought() {
        let mut b = Builder::new();
        b.reasoning("first", at(0));
        b.call("c1");
        b.reasoning("second ", at(30));
        b.reasoning("thought", at(32));

        let thoughts: Vec<&Part> = b
            .parts()
            .iter()
            .filter(|p| matches!(p, Part::Reasoning { .. }))
            .collect();
        assert!(
            matches!(thoughts[0], Part::Reasoning { ms: None, .. }),
            "one fragment has no span, so nothing timed it"
        );
        assert!(
            matches!(thoughts[1], Part::Reasoning { ms: Some(2000), .. }),
            "two seconds, not the thirty since the first: {:?}",
            thoughts[1]
        );
    }

    #[test]
    fn prose_after_a_thought_opens_a_new_part() {
        let mut b = Builder::new();
        b.reasoning("thinking", at(0));
        b.text("the answer");
        assert_eq!(b.parts().len(), 2);
        assert!(matches!(b.parts()[1], Part::Text { .. }));
    }

    #[test]
    fn a_thought_alone_still_counts_as_something_said() {
        let mut b = Builder::new();
        b.reasoning("deliberating", None);
        assert!(b.said_something());
    }

    #[test]
    fn nothing_and_empty_words_count_as_nothing() {
        assert!(!Builder::new().said_something());
        let mut b = Builder::new();
        b.text("");
        assert!(!b.said_something());
    }

    /// A kind from another build is kept, so a rolling deploy can read its own
    /// rows, but is never mistaken for speech.
    #[test]
    fn a_kind_from_another_build_survives_a_round_trip() {
        let stored = serde_json::json!([{"type": "hologram", "text": "hi"}]);
        let parts: Vec<Part> = serde_json::from_value(stored).expect("decode");
        assert_eq!(parts, [Part::Unknown]);
    }
}
