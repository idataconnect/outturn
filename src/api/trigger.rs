//! Starting a turn nobody asked for.
//!
//! Shared by every trigger, because there is exactly one right way to do it
//! and the last time it was written twice three of its steps went missing from
//! the copy: the `live_sessions` write the autoscaler reads, the `account` the
//! usage ledger groups by, and the event a browser needs to see the message
//! arrive.
//!
//! What every caller here has in common is the thing that makes it a trigger:
//! nobody is waiting. The turn's `user_id` is null, which the ledger expects of
//! work nobody sent and which stops `worker::inhibited` clearing a stopped
//! session's latch -- so an agent cannot restart itself by being triggered.

use serde_json::json;
use uuid::Uuid;

use crate::jobs;

/// What started this turn, for the session it creates.
#[derive(Debug, Clone, Copy)]
pub enum Source {
    Schedule(Uuid),
    Webhook(Uuid),
}

impl Source {
    fn id(&self) -> Uuid {
        match self {
            Source::Schedule(id) | Source::Webhook(id) => *id,
        }
    }
}

/// Everything one triggered turn needs.
pub struct Started {
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    /// What the session is called. A list of "Untitled" is no use to somebody
    /// working out what their agent did overnight.
    pub title: String,
    /// The message the turn begins with, stored with no `user_id`.
    pub prompt: String,
    /// The workspace's label for whose work this is, for the usage ledger.
    pub account: Option<String>,
    /// The zone the agent's clock should read in.
    pub timezone: Option<String>,
    pub source: Source,
    /// Anything else worth recording on the message, merged with what this
    /// function records about the trigger itself.
    pub metadata: serde_json::Value,
}

/// Creates the session and queues the turn, in one transaction.
///
/// Together, for the same reason `sessions::enqueue_turn` does it: a message
/// that exists with no job to answer it is a message nothing will ever notice.
pub async fn start(pool: &sqlx::PgPool, started: Started) -> Result<Uuid, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    let session_id = Uuid::now_v7();
    // The source column differs and nothing else does, so the statement is
    // written twice rather than built from a string -- sqlx takes literals,
    // and two short literals are cheaper to read than a builder.
    //
    // Matched on the enum rather than on a string it maps to, so a third kind
    // of trigger fails to compile here rather than compiling and writing its
    // id into the wrong column. An earlier version went through a
    // `&'static str` with a catch-all arm, which threw away exactly the check
    // this file was extracted to provide -- and docs/triggers.md already
    // contemplates email as a third kind.
    let inserted = match started.source {
        Source::Schedule(_) => sqlx::query(
            "insert into agent_sessions \
                 (id, workspace_id, agent_id, user_id, title, account, schedule_id) \
             values ($1, $2, $3, null, $4, $5, $6)",
        ),
        Source::Webhook(_) => sqlx::query(
            "insert into agent_sessions \
                 (id, workspace_id, agent_id, user_id, title, account, webhook_trigger_id) \
             values ($1, $2, $3, null, $4, $5, $6)",
        ),
    };
    inserted
        .bind(session_id)
        .bind(started.workspace_id)
        .bind(started.agent_id)
        .bind(&started.title)
        .bind(started.account.as_deref())
        .bind(started.source.id())
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    // A user message with no user. The role is what the model needs to see --
    // a turn with nothing in the user position has nothing to answer -- while
    // the null `user_id` is what says nobody typed it. The browser draws it as
    // the trigger's words rather than as somebody's.
    let message_id = Uuid::now_v7();
    sqlx::query(
        "insert into agent_messages (id, session_id, role, content, user_id, metadata) \
         values ($1, $2, 'user', $3, null, $4)",
    )
    .bind(message_id)
    .bind(session_id)
    .bind(&started.prompt)
    .bind(&started.metadata)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // The struct rather than a JSON literal, so a field added to
    // `ChatTurnPayload` fails here at compile time rather than breaking every
    // triggered turn at runtime in a loop nobody is watching.
    let payload = serde_json::to_value(crate::api::worker::ChatTurnPayload {
        workspace_id: started.workspace_id,
        session_id,
        agent_id: started.agent_id,
        message_id,
        timezone: started.timezone,
        user_id: None,
    })
    .map_err(|e| e.to_string())?;

    // Somebody's agent is in this conversation even though nobody is, so it
    // counts towards how many pods the fleet wants. `desired_runtime_pods`
    // reads recently active sessions because queue depth is a lagging measure,
    // and a triggered turn left out of it is one the autoscaler cannot see
    // coming.
    sqlx::query(
        "insert into live_sessions (session_id, expires_at) \
         values ($1, now() + interval '5 minutes') \
         on conflict (session_id) do update set expires_at = excluded.expires_at",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    jobs::enqueue(
        &mut *tx,
        started.workspace_id,
        crate::api::worker::CHAT_TURN,
        payload,
        None,
        Some(&session_id.to_string()),
        // Nobody is waiting, so this queues behind anyone who is.
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .map_err(|e| e.to_string())?;

    // Announced like any other message. A browser with the session open reads
    // history plus a cursor and then consumes events above it, so a message
    // that arrives with no event is one the reader never sees appear -- and
    // the deltas of the reply that follows hang off nothing.
    crate::events::append_on(
        &mut tx,
        started.workspace_id,
        Some(session_id),
        "chat.message",
        json!({
            "id": message_id,
            "session_id": session_id,
            "role": "user",
            "content": started.prompt,
        }),
    )
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(session_id)
}
