pub mod parts;
mod postgres;
pub mod summarize;
pub mod trim;

pub use postgres::PostgresChatStore;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A latch that was lifted: what stopped it, and when it was stopped.
#[derive(Debug, Clone)]
pub struct Stopped {
    pub reason: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSession {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    /// Who started it. Null for a session the platform made, and kept when an
    /// account goes away so a transcript is not silently reattributed. What
    /// makes "may use this agent, may not read what others said to it"
    /// expressible: your own conversations are yours whoever else may not read
    /// them.
    pub user_id: Option<Uuid>,
    pub title: String,
    /// Which of the workspace's own customers this conversation is for, in the
    /// workspace's own terms. Copied onto every usage row the session produces
    /// so the workspace can split its bill; meaningless to the platform.
    pub account: Option<String>,
    /// When a message was last stored in it. What the recent list orders by.
    pub last_active_at: DateTime<Utc>,
    /// The state of its live turn -- `pending`, `running` or `parked` -- or
    /// none when nothing is in flight.
    pub turn: Option<String>,
}

/// A place in the recent list: the last session seen, by the two values the
/// list is ordered on. Opaque to a client, which hands back what it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recent {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

impl Recent {
    pub fn of(session: &AgentSession) -> Self {
        Self {
            at: session.last_active_at,
            id: session.id,
        }
    }

    /// Microseconds, since that is what Postgres keeps: a coarser cursor
    /// would land between two sessions active in the same millisecond.
    pub fn encode(&self) -> String {
        format!("{}.{}", self.at.timestamp_micros(), self.id)
    }

    pub fn decode(cursor: &str) -> Option<Self> {
        let (micros, id) = cursor.split_once('.')?;
        Some(Self {
            at: DateTime::from_timestamp_micros(micros.parse().ok()?)?,
            id: id.parse().ok()?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: String,
    pub content: String,
    /// What the agent did to produce this: `{"tool_calls": [...]}`, empty
    /// when it simply answered.
    pub metadata: serde_json::Value,
    /// How many `chat.delta` fragments this content already accounts for.
    ///
    /// A reply is created empty and streamed into, so while a turn is running
    /// the content is assembled from deltas rather than stored. This is where
    /// the next delta belongs, which is what lets a client resume a stream it
    /// reconnected to -- and refuse a fragment it has already folded in.
    pub delta_next: i32,
    pub model: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    /// On a reply, the user message it answers. What lets a reader pair a
    /// prompt with the reply being written for it, and so say where a
    /// message is in its life rather than showing an empty bubble.
    pub replies_to: Option<Uuid>,
    /// Which attempt at answering its prompt this reply is. 1 for everything
    /// that never had to be run twice, which is nearly everything.
    #[serde(default)]
    pub attempt: i32,
    /// When this stopped being written, as distinct from when it was created.
    ///
    /// A reply's id is assigned when its placeholder is made, at the start of
    /// the turn -- right to order by, wrong to show. A turn that waited
    /// thirteen minutes for an approval produced a reply that said "13 minutes
    /// ago" the moment it finished streaming.
    ///
    /// Null while a reply is still being written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// On a user message, the reply that took it mid-turn. Such a message is
    /// answered inside that reply and never gets one of its own.
    pub absorbed_by: Option<Uuid>,
    /// On a user message, the state of the job answering it -- pending,
    /// running, succeeded or failed. Only the transcript read fills this;
    /// live changes arrive as events.
    pub job_state: Option<String>,
}

/// A session's transcript, with the event cursor it was read at.
///
/// The two come from one statement, so they share a snapshot: every event at
/// or below `cursor` is already reflected in `messages`, and everything above
/// it is still to come over the feed. Polling from here is what stops history
/// and stream from overlapping -- the bug that appended a message's own text
/// to itself on reload.
#[derive(Debug, Clone, Serialize)]
pub struct History {
    pub messages: Vec<Message>,
    pub cursor: Uuid,
    /// Whether anything older than the first message here was left behind.
    ///
    /// Answered from the same statement as the rows, so it cannot disagree
    /// with them: a caller told `false` will not find a "load earlier" button
    /// that fetches nothing, and one told `true` is not guessing from a full
    /// page that happened to land on the boundary.
    #[serde(default)]
    pub has_more: bool,
    /// The approval this conversation is waiting on, if it is waiting on one.
    ///
    /// Served with the history for the same reason the cursor is: a reader who
    /// reloads has to learn everything that is true *now* from one answer. The
    /// banner and the approval card were driven only by the live `chat.held`
    /// event, so a tab that was not open when the turn parked -- or was
    /// reloaded after -- showed a conversation that had simply stopped, with
    /// the question it was waiting on nowhere on screen.
    ///
    /// Null when nothing is pending, which is nearly always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting: Option<serde_json::Value>,
    /// The sleep this conversation is in, if the agent is asleep. See
    /// `wake::asleep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asleep: Option<serde_json::Value>,
}

/// How a message reaches a turn that is already running.
///
/// Only meaningful while something is in flight. A message arriving into a
/// quiet session starts a turn regardless of which this is.
/// The empty reply a turn streams into, and whether this turn created it.
///
/// A turn can be attempted more than once -- a worker died, a runtime had no
/// room -- and the reply is deliberately idempotent so a retry takes back the
/// one it already made. But announcing it is not idempotent: telling the
/// browser again on every attempt writes an event per attempt for a message it
/// already has, and a turn that keeps being handed back writes them forever.
pub struct Placeholder {
    pub message: Message,
    pub created: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Injected at the next round boundary, redirecting work in progress.
    Steer,
    /// Held until the agent would otherwise stop, then extends the turn.
    FollowUp,
}

/// What a settled approval left behind in the conversation.
pub struct ApprovalAnswer<'a> {
    /// The act, as the gate declared it: `charge`, `reach`.
    pub requires: &'a str,
    /// False for a decline, which stops the turn rather than resuming it and is
    /// exactly as worth recording.
    pub approved: bool,
    /// Who answered. Absent only where the answer came from something other
    /// than a person, which nothing does today.
    pub answered_by: Option<Uuid>,
    /// Their name, resolved once here rather than by every reader of the
    /// transcript later.
    pub answered_by_name: Option<&'a str>,
    /// What they wanted recorded, where they said anything.
    pub note: Option<&'a str>,
}

/// The metadata key marking a message as the record of an approval.
///
/// Named like `summarize::SUMMARY_MARK` and read the same way: the client
/// switches on its presence to draw the message as a boundary rather than as
/// something the agent said.
pub const APPROVAL_MARK: &str = "approval";

/// What a declined agent is told, beside the decline itself.
///
/// A decline lifts the hold, so the conversation carries on and the turn is
/// free to run. Nothing mechanical then stops the agent making the same call
/// again and raising the same question a second later -- so it is told not to,
/// in the transcript, where it reads as what a person decided rather than as a
/// tool failing.
///
/// Deliberately not "never do this". Somebody who declines a charge and then
/// talks it over should be able to change their mind and have it go through,
/// which means the agent has to be free to try again once that conversation has
/// happened. The gate is what holds the line regardless: a decline mints no
/// grant, so any retry raises a fresh approval rather than slipping past.
pub const DECLINED_GUIDANCE: &str = "Do not re-attempt this before discussing it with the user.";

impl Default for Delivery {
    /// Steering is the default because it is what someone typing during a
    /// turn almost always means: they are reacting to what they can see.
    fn default() -> Self {
        Self::Steer
    }
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateSession {
    pub agent_id: Uuid,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub account: Option<String>,
}

/// Token counts reported by a provider, recorded per message so usage can be
/// attributed to a workspace later.
///
/// Reported rather than counted. The breakdown is where the price differences
/// live -- cached input is cheaper, thinking is billed as output -- and none
/// of it is recoverable from the text, whatever tokenizer you own. A local
/// count has its uses, but they are sizing and budgeting, never accounting.
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub cache_read_tokens: Option<i32>,
    pub cache_write_tokens: Option<i32>,
    pub reasoning_tokens: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("session not found")]
    NotFound,
    /// An empty assistant message exists that no live job is filling, so a
    /// turn died without cleaning up. Refused loudly rather than written
    /// around: an empty message is replayed to the model on every later turn,
    /// and silently tolerating one hides the failure that produced it.
    #[error("session {0} has an abandoned empty reply; refusing to write past it")]
    Abandoned(Uuid),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("chat store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait ChatStore: Send + Sync {
    async fn create_session(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
        input: CreateSession,
    ) -> Result<AgentSession, ChatError>;

    async fn list_sessions(
        &self,
        workspace_id: Uuid,
        agent_ids: Option<&[Uuid]>,
        user_id: Uuid,
        agent: Option<Uuid>,
        title: Option<&str>,
        after: Option<Recent>,
        limit: i64,
    ) -> Result<Vec<AgentSession>, ChatError>;

    async fn get_session(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
    ) -> Result<AgentSession, ChatError>;

    async fn delete_session(&self, workspace_id: Uuid, session_id: Uuid) -> Result<(), ChatError>;

    /// Gives a session its name.
    ///
    /// Empty means unnamed, which is what a session starts as and what the
    /// namer looks for; a person clearing the field hands it back to them.
    async fn rename_session(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        title: &str,
    ) -> Result<AgentSession, ChatError>;

    /// The whole transcript, oldest first.
    ///
    /// What a turn is built from: the model needs every message, so this has
    /// no limit and is not what a reader should be served.
    async fn messages(&self, session_id: Uuid) -> Result<History, ChatError>;

    /// What a turn's projection will use, and no more: the newest summary and
    /// the messages after the one it covers, which is exactly what
    /// `projected_with_sources` keeps. Everything a summary stands in for is
    /// left unread, so building a turn costs the tail rather than the whole
    /// session however long it has run. With no summary it is `messages`.
    async fn turn_history(&self, session_id: Uuid) -> Result<History, ChatError>;

    /// A page of the transcript, oldest first, ending at the newest message.
    ///
    /// `before` is a keyset cursor: the page holds the `limit` messages
    /// immediately older than it, and absent means "the newest `limit`". Ids
    /// are UUIDv7, so this is also paging backwards through time, and the
    /// column it seeks on is the one `agent_messages (session_id, id)` is
    /// already ordered by.
    async fn messages_page(
        &self,
        session_id: Uuid,
        before: Option<Uuid>,
        limit: i64,
    ) -> Result<History, ChatError>;

    /// Replaces a message's content, for a reply that was created empty and
    /// streamed into.
    ///
    /// Usage is recorded here rather than when the reply was created, because
    /// what a turn cost is only known once it has finished.
    async fn set_message_content(
        &self,
        message_id: Uuid,
        content: &str,
        model: Option<&str>,
        provider: Option<&str>,
        usage: Usage,
        metadata: serde_json::Value,
    ) -> Result<Message, ChatError>;

    /// Removes a message. Used to clear a placeholder whose turn failed, which
    /// would otherwise sit empty in the transcript forever.
    async fn delete_message(&self, message_id: Uuid) -> Result<(), ChatError>;

    /// The empty assistant message a job streams into, created once.
    ///
    /// A job that is retried must fill the message it already created rather
    /// than making another: a worker killed mid-turn never runs its own
    /// cleanup, so a second placeholder would leave the first behind forever.
    /// Ownership is recorded on the job, so concurrent turns in one session
    /// cannot claim each other's.
    async fn claim_placeholder(
        &self,
        replies_to: Uuid,
        session_id: Uuid,
        // Which attempt this is. A crashed turn retries its own attempt and
        // takes its row back; a turn resuming after an approval asks for the
        // next one, so the refused reply stays in the transcript.
        attempt: i32,
    ) -> Result<Placeholder, ChatError>;

    /// Whether this prompt was already answered inside an earlier turn.
    ///
    /// A steered message is absorbed by the reply it interrupted, so the turn
    /// queued for it must not answer it a second time.
    async fn was_absorbed(&self, message_id: Uuid) -> Result<bool, ChatError>;

    /// Latches a session as stopped, with what to tell the next turn.
    ///
    /// Separate from the inhibitor that caused it because the two outlive each
    /// other: releasing the hold must not resume anything. See
    /// `docs/inhibitors.md`.
    async fn stop_session(&self, session_id: Uuid, reason: &str) -> Result<(), ChatError>;

    /// Lifts the latch, for a prompt that carries a real `user_id`.
    ///
    /// Returns what the session was stopped for, where it was stopped, so the
    /// turn that clears it can say so rather than starting in silence.
    /// Lifts the latch, returning why it was stopped and when.
    ///
    /// The time matters as much as the reason. An agent resuming after three
    /// weeks that is told only "the workspace was stopped" will carry on from
    /// what it last said as though no time passed, stating balances and
    /// deadlines it has no current basis for.
    async fn clear_stop(&self, session_id: Uuid) -> Result<Option<Stopped>, ChatError>;

    /// Why this session is stopped, if it is.
    /// Why this session is latched, if it is.
    ///
    /// The reason belongs to the stop that took the latch, not to whatever
    /// hold covers the session now -- a later hold did not stop anything, and
    /// telling a reader its reason contradicts the transcript, the latch and
    /// the marker the next turn will carry. `stop_session` preserves the first
    /// by refusing to write where `stopped_at` is set, and callers announcing
    /// a refusal read it from here rather than from the current verdict.
    async fn stopped_reason(&self, session_id: Uuid) -> Result<Option<String>, ChatError>;

    /// Discards a job's placeholder, for a turn that will never be retried.
    ///
    /// Without this a permanently failed turn leaves an empty message that
    /// `append_message` then refuses to follow, which would wedge the session.
    async fn discard_placeholder(&self, replies_to: Uuid, attempt: i32) -> Result<(), ChatError>;

    /// Which attempt a turn about to run should write as.
    ///
    /// The latest attempt this prompt already has, or the one after it when
    /// that attempt is finished and this turn is resuming rather than retrying.
    /// Derived here rather than counted by the caller, because two call sites
    /// claim the same placeholder for one turn and a number computed twice is a
    /// number that can differ -- which would have a turn write a second reply
    /// halfway through itself.
    ///
    /// `resuming` is what tells them apart. A crashed turn retries its own
    /// attempt and takes its row back; a turn given back after somebody answered
    /// an approval starts a new one, so the refused reply stays in the
    /// transcript with the question the reader approved against.
    async fn attempt_for(&self, replies_to: Uuid, resuming: bool) -> Result<i32, ChatError>;

    /// The attempt this prompt's latest reply is, deciding nothing.
    ///
    /// For callers after `attempt_for` has already chosen: the report that
    /// streams into the reply, and the path that gives up on it. Asking
    /// `attempt_for` again there is asking it to decide again -- and a turn
    /// mid-stream looks exactly like one interrupted, so it would seal its own
    /// reply and move on to a second one halfway through itself.
    async fn current_attempt(&self, replies_to: Uuid) -> Result<i32, ChatError>;

    /// Records that somebody answered an approval, in the conversation it was
    /// about.
    ///
    /// A message rather than an event, because an event is swept and this is the
    /// audit trail: a person authorized a payment, and the place somebody would
    /// look for that is the conversation. The grant row records it too, but
    /// nobody reads grant rows to find out what happened in a chat.
    ///
    /// Marked rather than written as ordinary prose, the way a summary is: the
    /// client draws it as the boundary it is, and a reader who reloads sees that
    /// the conversation paused and why it carried on -- which is otherwise
    /// invisible, since the banner is live-only and leaves no trace.
    ///
    /// Also what reconciles the refused tool call above it. Without this the
    /// transcript keeps a failed `POST /charges` that was later approved and
    /// succeeded, with nothing joining them, and reads as a failure followed by
    /// an unexplained success.
    async fn record_approval(
        &self,
        session_id: Uuid,
        answer: ApprovalAnswer<'_>,
    ) -> Result<Message, ChatError>;

    /// Appends a message, assigning the next sequence number for the session.
    async fn append_message(
        &self,
        session_id: Uuid,
        role: &str,
        content: &str,
        model: Option<&str>,
        usage: Usage,
        delivery: Delivery,
        // Who sent it, for a message with an author. None for anything the
        // system produced.
        user_id: Option<Uuid>,
    ) -> Result<Message, ChatError>;
}

#[cfg(test)]
mod recent_tests {
    use super::*;

    #[test]
    fn a_cursor_survives_the_round_trip_to_the_microsecond() {
        let at = DateTime::from_timestamp_micros(1_791_050_000_123_456).unwrap();
        let recent = Recent {
            at,
            id: Uuid::now_v7(),
        };
        assert_eq!(Recent::decode(&recent.encode()), Some(recent));
    }

    #[test]
    fn a_cursor_that_is_not_one_is_refused() {
        for bad in ["", "123", "abc.def", "12.not-a-uuid", "01a1-uuid-looking"] {
            assert_eq!(Recent::decode(bad), None, "{bad}");
        }
    }
}
