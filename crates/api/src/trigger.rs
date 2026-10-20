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

use outturn_core::jobs;

/// What started this turn, for the session it creates.
#[derive(Debug, Clone, Copy)]
pub enum Source {
    Schedule(Uuid),
    Webhook(Uuid),
    Email(Uuid),
}

impl Source {
    fn id(&self) -> Uuid {
        match self {
            Source::Schedule(id) | Source::Webhook(id) | Source::Email(id) => *id,
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
    let session_id = start_in(&mut tx, started).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(session_id)
}

// -- The pipeline every trigger fires through ------------------------------------

/// What a source hands on when its trigger fires: what arrived, and what the
/// sender calls it. See docs/triggers.md, "One model for every trigger".
pub struct Firing {
    /// What arrived, put where the prompt says `{{body}}`. None for a firing
    /// that brings nothing, as a schedule's does.
    pub text: Option<String>,
    /// What the sender calls this firing, so the same one delivered twice
    /// runs once. None when the sender names nothing.
    pub external_id: Option<String>,
    /// The zone the agent's clock should read in.
    pub timezone: Option<String>,
    /// Recorded on the message, beside what `start` records.
    pub metadata: serde_json::Value,
}

/// The trigger a firing is for, as its source read it.
pub struct Firer {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub name: String,
    pub prompt: String,
    pub account: Option<String>,
    pub source: Source,
}

/// How a firing went.
#[derive(Debug)]
pub enum Fired {
    Started(Uuid),
    /// The sender's id for it was seen before; nothing was started.
    Duplicate,
    /// The trigger's ceiling turned it away; nothing was started.
    OverCeiling(Ceiling),
    Failed(String),
}

/// Which half of a trigger's ceiling turned a firing away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceiling {
    /// Its firings this hour.
    Hourly,
    /// Its turns not yet finished.
    Concurrent,
}

impl Ceiling {
    /// Why, in words, for a record somebody reads.
    pub fn why(self) -> &'static str {
        match self {
            Ceiling::Hourly => "the trigger's hourly ceiling was reached",
            Ceiling::Concurrent => "the trigger already has as many turns running as it may",
        }
    }
}

/// What the body is substituted into.
pub const BODY_PLACEHOLDER: &str = "{{body}}";

/// Fires a trigger: refuses what was seen before, counts it against the
/// ceiling, writes the prompt, and starts the turn -- the same steps in the
/// same order for every kind, so none of them is a kind's to forget.
///
/// Seen before is checked first, so a duplicate does not spend the hourly
/// allowance a real firing needs, and again in the transaction that starts
/// the turn, where the record is written: two copies racing are then one turn
/// and one `Duplicate`, never two turns.
pub async fn fire(pool: &sqlx::PgPool, firer: &Firer, firing: Firing) -> Fired {
    if let Some(external) = &firing.external_id {
        let seen: Result<Option<i64>, _> = sqlx::query_scalar(
            "select 1::bigint from trigger_firings where trigger_id = $1 and external_id = $2",
        )
        .bind(firer.id)
        .bind(external)
        .fetch_optional(pool)
        .await;
        match seen {
            Ok(Some(_)) => return Fired::Duplicate,
            Ok(None) => {}
            Err(e) => return Fired::Failed(e.to_string()),
        }
    }

    let prompt = match &firing.text {
        Some(text) => prompt_for(&firer.prompt, text),
        None => firer.prompt.clone(),
    };
    let started = Started {
        workspace_id: firer.workspace_id,
        agent_id: firer.agent_id,
        title: firer.name.clone(),
        prompt,
        account: firer.account.clone(),
        timezone: firing.timezone,
        source: firer.source,
        metadata: firing.metadata,
    };

    let run = async {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        // Both halves of the ceiling under one lock on the trigger, so two
        // firings arriving together cannot each count the other as not yet
        // started -- and a firing the concurrent half refuses is not counted
        // against the hour.
        if let Some(ceiling) = over_ceiling(&mut tx, firer.id)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(Err(ceiling));
        }
        if let Some(external) = &firing.external_id {
            let recorded = sqlx::query(
                "insert into trigger_firings (trigger_id, external_id) values ($1, $2) \
                 on conflict do nothing",
            )
            .bind(firer.id)
            .bind(external)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            if recorded.rows_affected() == 0 {
                return Ok(Ok(None));
            }
        }
        let session_id = start_in(&mut tx, started).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok::<_, String>(Ok(Some(session_id)))
    };
    match run.await {
        Ok(Ok(Some(session_id))) => Fired::Started(session_id),
        Ok(Ok(None)) => Fired::Duplicate,
        Ok(Err(ceiling)) => Fired::OverCeiling(ceiling),
        Err(e) => Fired::Failed(e),
    }
}

/// The prompt a firing produces.
///
/// What arrived goes where the template says, and nowhere else. A template
/// with no placeholder is a trigger whose author wants the same prompt every
/// time, which is unusual but not wrong -- the firing is then only a signal
/// that something happened.
pub fn prompt_for(template: &str, body: &str) -> String {
    template.replace(BODY_PLACEHOLDER, body)
}

/// Counts a firing against its trigger's hourly ceiling, and says whether it
/// fits. A trigger with no ceiling always fits.
///
/// One statement, on the trigger's own row. In the same database as the work
/// it bounds rather than in a cache, because this is a refusal and not a
/// metric: a ceiling that fails open when its store is unreachable is not a
/// ceiling. A fixed window, which admits up to twice the limit across a
/// boundary -- the honest cost of one row and one statement, and twice the
/// limit is still not a runaway. A refused firing is not counted, or a sender
/// ignoring refusals would hold the window open for ever.
/// Which half of the ceiling refuses a firing now, if either does: its turns
/// not yet finished, then its firings this hour -- the hour counted only for a
/// firing the first half let through. Locks the trigger for the transaction.
async fn over_ceiling(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Ceiling>, sqlx::Error> {
    let max: Option<i32> =
        sqlx::query_scalar("select max_concurrent from triggers where id = $1 for update")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    if let Some(max) = max {
        // Unfinished as `sessions.rs` reads it: a state added to jobs has to
        // be added here too (docs/invariants.md).
        let running: i64 = sqlx::query_scalar(
            "select count(*) from jobs j \
               join agent_sessions s on s.id = (j.payload->>'session_id')::uuid \
              where s.trigger_id = $1 and j.kind = 'chat.turn' \
                and j.state in ('pending', 'running', 'parked')",
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
        if running >= i64::from(max) {
            return Ok(Some(Ceiling::Concurrent));
        }
    }
    if !admit_in(&mut **tx, id).await? {
        return Ok(Some(Ceiling::Hourly));
    }
    Ok(None)
}

pub async fn admit(pool: &sqlx::PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    admit_in(pool, id).await
}

async fn admit_in<'e, E: sqlx::PgExecutor<'e>>(executor: E, id: Uuid) -> Result<bool, sqlx::Error> {
    let admitted = sqlx::query(
        "update triggers set \
             window_start = case when window_start < now() - interval '1 hour' \
                                 then now() else window_start end, \
             window_count = case when window_start < now() - interval '1 hour' \
                                 then 1 else window_count + 1 end \
         where id = $1 \
           and (max_per_hour is null \
                or window_start < now() - interval '1 hour' or window_count < max_per_hour)",
    )
    .bind(id)
    .execute(executor)
    .await?;
    Ok(admitted.rows_affected() > 0)
}

/// As `start`, inside a transaction the caller commits -- so whatever else a
/// firing records lands with its turn or not at all.
async fn start_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    started: Started,
) -> Result<Uuid, String> {
    let session_id = Uuid::now_v7();
    // Matched on the enum rather than on a string it maps to, so a third kind
    // of trigger fails to compile here until it says what it is called. An
    // earlier version went through a `&'static str` with a catch-all arm,
    // which threw away exactly the check this file was extracted to provide.
    let started_by = match started.source {
        Source::Schedule(_) => "schedule",
        Source::Webhook(_) => "webhook",
        Source::Email(_) => "email",
    };
    sqlx::query(
        "insert into agent_sessions \
             (id, workspace_id, agent_id, user_id, title, account, trigger_id, started_by) \
         values ($1, $2, $3, null, $4, $5, $6, $7)",
    )
    .bind(session_id)
    .bind(started.workspace_id)
    .bind(started.agent_id)
    .bind(&started.title)
    .bind(started.account.as_deref())
    .bind(started.source.id())
    .bind(started_by)
    .execute(&mut **tx)
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
    .execute(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;

    // The struct rather than a JSON literal, so a field added to
    // `ChatTurnPayload` fails here at compile time rather than breaking every
    // triggered turn at runtime in a loop nobody is watching.
    let payload = serde_json::to_value(crate::worker::ChatTurnPayload {
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
    .execute(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;

    jobs::enqueue(
        &mut **tx,
        started.workspace_id,
        crate::worker::CHAT_TURN,
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
    outturn_core::events::append_on(
        tx,
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

    Ok(session_id)
}
