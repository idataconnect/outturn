//! Waking an agent later: `sleep`, which pauses the conversation, and
//! `set-timer`, which does not.
//!
//! Two tools with one mechanism behind them, because the choice between them is
//! one a model reasons about -- should the conversation stop while I wait, or
//! carry on -- and not because they need different machinery. Both end in the
//! same place: a note the platform writes into the conversation when the wait is
//! over, which becomes the prompt of a turn that answers it.
//!
//! Nothing is held open while an agent waits. A wakeup is a job with a due time
//! (`WAKE`); the turn that asked ends normally and its runtime slot is freed.
//!
//! A sleep also pauses the conversation, with the machinery an approval already
//! uses: a suspended hold, so anything said meanwhile parks, and an item in the
//! owner's queue, so a person can see it and end it early. Of the two ways a
//! sleep can end -- the wakeup coming due, somebody pressing Wake now -- only the
//! one that wins the settle writes the note, because the note is written inside
//! the settle's transaction.
//!
//! On waking, the messages that arrived while the agent slept are answered by the
//! wake turn together rather than each by a turn of its own: they are marked
//! answered by the note in the same transaction, so when the parked turns are
//! given back they find their messages taken and finish without a word. The note
//! sits after them -- the honest order -- and says how long before the wake each
//! arrived, which stays true however long the turn takes to start.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use super::inhibitor::InhibitorStore;
use super::router::{ApiError, ApiState};
use crate::auth::Authority;
use crate::jobs;

/// The job kind a wakeup is due as.
pub const WAKE: &str = "chat.wake";

/// The kind of queue item a sleep raises.
pub const SLEEP_ITEM: &str = "sleep";

/// Who took a sleep's hold, so one can be told apart from an approval's.
pub const SLEEP_HOLDER: &str = "sleep";

/// Where the wake note records what it is, in the message's metadata.
pub const WAKE_MARK: &str = "wake";

/// The longest wait either tool accepts. Long enough for anything an agent is
/// plausibly waiting on, short enough that a wakeup is not lost for a year.
pub const LONGEST: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A reason longer than this is not a reason.
const LONGEST_REASON: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Sleep,
    Timer,
}

/// What a wakeup job carries until it is due.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Due {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
    pub kind: Kind,
    pub reason: String,
    pub set_at: chrono::DateTime<chrono::Utc>,
    pub due_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub timezone: Option<String>,
    /// For a sleep: the queue item and the hold it stands for.
    #[serde(default)]
    pub item_id: Option<Uuid>,
    #[serde(default)]
    pub inhibitor_id: Option<Uuid>,
}

/// The note a wait ends with, as it is written.
#[derive(Debug, Clone)]
pub struct Note {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
    pub kind: Kind,
    pub reason: String,
    pub set_at: chrono::DateTime<chrono::Utc>,
    /// When it was due, so a sleep ended early can say so.
    pub due_at: chrono::DateTime<chrono::Utc>,
    pub timezone: Option<String>,
    /// The person who pressed Wake now, where one did.
    pub woken_by: Option<String>,
}

impl Note {
    fn of(due: &Due, woken_by: Option<String>) -> Self {
        Self {
            workspace_id: due.workspace_id,
            session_id: due.session_id,
            agent_id: due.agent_id,
            kind: due.kind,
            reason: due.reason.clone(),
            set_at: due.set_at,
            due_at: due.due_at,
            timezone: due.timezone.clone(),
            woken_by,
        }
    }
}

/// A span of time the way a person says it: "45 seconds", "8 minutes", "2 hours".
pub fn span(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (n, unit) = if seconds < 60 {
        (seconds, "second")
    } else if seconds < 90 * 60 {
        ((seconds + 30) / 60, "minute")
    } else if seconds < 36 * 60 * 60 {
        ((seconds + 1800) / 3600, "hour")
    } else {
        ((seconds + 43_200) / 86_400, "day")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// A moment as the agent's user would read it: their zone where known.
fn clock(at: chrono::DateTime<chrono::Utc>, timezone: Option<&str>) -> String {
    match timezone.and_then(|z| z.parse::<chrono_tz::Tz>().ok()) {
        Some(tz) => at.with_timezone(&tz).format("%H:%M %Z").to_string(),
        None => at.format("%H:%M UTC").to_string(),
    }
}

/// A moment in full, as the confirmation says it: "Tuesday 29 September at
/// 3:12 pm PDT". The day is named because a timer is as often tomorrow as
/// today, and "3:12 pm" alone leaves the reader to guess which.
fn when(at: chrono::DateTime<chrono::Utc>, timezone: Option<&str>) -> String {
    const FORMAT: &str = "%A %-d %B at %-I:%M %P";
    match timezone.and_then(|z| z.parse::<chrono_tz::Tz>().ok()) {
        Some(tz) => {
            let local = at.with_timezone(&tz);
            format!("{} {}", local.format(FORMAT), local.format("%Z"))
        }
        None => format!("{} UTC", at.format(FORMAT)),
    }
}

/// The words the agent wakes to.
///
/// Written for a model, so it says what happened in the order that matters --
/// how long, why, what arrived meanwhile -- and what to do about it. Each queued
/// message's time is given relative to the wake rather than to now: "8 minutes
/// before you woke" stays true however long this waits to be read, where "8
/// minutes ago" would not. The messages keep their own words; only this note
/// speaks about timing, since rewriting what somebody said to add a timestamp
/// would put words in their mouth.
pub fn compose(
    note: &Note,
    woke_at: chrono::DateTime<chrono::Utc>,
    queued: &[chrono::DateTime<chrono::Utc>],
) -> String {
    let zone = note.timezone.as_deref();
    let mut out = match note.kind {
        Kind::Sleep => {
            let mut s = format!(
                "You slept from {} to {} ({}): {}",
                clock(note.set_at, zone),
                clock(woke_at, zone),
                span((woke_at - note.set_at).num_seconds()),
                note.reason.trim(),
            );
            if !s.ends_with(['.', '!', '?']) {
                s.push('.');
            }
            if let Some(who) = &note.woken_by {
                s.push_str(&format!(
                    " {who} woke you early; you had asked to sleep until {}.",
                    clock(note.due_at, zone)
                ));
            }
            s
        }
        Kind::Timer => {
            let mut s = format!(
                "A timer you set at {} has fired: {}",
                clock(note.set_at, zone),
                note.reason.trim(),
            );
            if !s.ends_with(['.', '!', '?']) {
                s.push('.');
            }
            s
        }
    };

    if !queued.is_empty() {
        let before: Vec<String> = queued
            .iter()
            .map(|at| span((woke_at - *at).num_seconds()))
            .collect();
        let list = match before.as_slice() {
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
            [] => unreachable!(),
        };
        let (count, verb) = if queued.len() == 1 {
            ("1 message".to_string(), "is")
        } else {
            (format!("{} messages", queued.len()), "are")
        };
        out.push_str(&format!(
            " {count} arrived while you slept and {verb} shown above, sent {list} \
             before you woke. Answer {}.",
            if queued.len() == 1 {
                "it"
            } else {
                "them together"
            }
        ));
    }
    out
}

/// Writes the note and hands it to a turn, on the caller's transaction.
///
/// For a sleep, the messages that arrived meanwhile are marked answered by the
/// note in the same statement set, so the parked turns the settle gives back
/// find them taken. For a timer nothing is marked: the conversation was never
/// paused, so anything unanswered has a turn of its own already running or due.
///
/// Returns the note's message id.
pub async fn write_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    note: Note,
) -> Result<Uuid, sqlx::Error> {
    let woke_at = chrono::Utc::now();

    // Unanswered, unabsorbed messages from people, in the order they arrived.
    let queued: Vec<(Uuid, chrono::DateTime<chrono::Utc>)> = if note.kind == Kind::Sleep {
        sqlx::query_as(
            "select u.id, u.created_at from agent_messages u \
             where u.session_id = $1 and u.role = 'user' and u.absorbed_by is null \
               and u.user_id is not null \
               and not exists (select 1 from agent_messages r where r.replies_to = u.id) \
             order by u.id \
             for update",
        )
        .bind(note.session_id)
        .fetch_all(&mut **tx)
        .await?
    } else {
        Vec::new()
    };

    let content = compose(
        &note,
        woke_at,
        &queued.iter().map(|(_, at)| *at).collect::<Vec<_>>(),
    );
    let metadata = serde_json::json!({
        WAKE_MARK: {
            "kind": note.kind,
            "reason": note.reason,
            "set_at": note.set_at,
            "due_at": note.due_at,
            "woke_at": woke_at,
            "woken_by": note.woken_by,
            "answers": queued.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        }
    });

    // No `user_id`: nobody sent it. The same rule a scheduled prompt follows,
    // and what stops a wake turn clearing a stopped session's latch.
    let note_id = Uuid::now_v7();
    sqlx::query(
        "insert into agent_messages (id, session_id, role, content, user_id, metadata) \
         values ($1, $2, 'user', $3, null, $4)",
    )
    .bind(note_id)
    .bind(note.session_id)
    .bind(&content)
    .bind(&metadata)
    .execute(&mut **tx)
    .await?;

    let ids: Vec<Uuid> = queued.iter().map(|(id, _)| *id).collect();
    if !ids.is_empty() {
        sqlx::query("update agent_messages set absorbed_by = $1 where id = any($2)")
            .bind(note_id)
            .bind(&ids)
            .execute(&mut **tx)
            .await?;
    }

    // Counted towards the pods the fleet wants, as a triggered turn is.
    sqlx::query(
        "insert into live_sessions (session_id, expires_at) \
         values ($1, now() + interval '5 minutes') \
         on conflict (session_id) do update set expires_at = excluded.expires_at",
    )
    .bind(note.session_id)
    .execute(&mut **tx)
    .await?;

    // Somebody who sent a message while the agent slept is waiting on the
    // answer; a wake with nobody waiting is background work like a schedule.
    let priority = if ids.is_empty() {
        jobs::PRIORITY_BACKGROUND
    } else {
        jobs::PRIORITY_REALTIME
    };
    let protocol = |e: String| sqlx::Error::Protocol(e);
    let payload = serde_json::to_value(super::worker::ChatTurnPayload {
        workspace_id: note.workspace_id,
        session_id: note.session_id,
        agent_id: note.agent_id,
        message_id: note_id,
        timezone: note.timezone.clone(),
        user_id: None,
    })
    .map_err(|e| protocol(e.to_string()))?;
    jobs::enqueue(
        &mut **tx,
        note.workspace_id,
        super::worker::CHAT_TURN,
        payload,
        None,
        Some(&note.session_id.to_string()),
        priority,
    )
    .await
    .map_err(|e| protocol(e.to_string()))?;

    // Told to whoever has the conversation open, as any new message is, and
    // each message it answers marked as answered by it.
    crate::events::append_on(
        tx,
        note.workspace_id,
        Some(note.session_id),
        "chat.message",
        serde_json::json!({
            "id": note_id,
            "session_id": note.session_id,
            "role": "user",
            "content": content,
            "metadata": metadata,
        }),
    )
    .await
    .map_err(|e| protocol(e.to_string()))?;
    for id in &ids {
        crate::events::append_on(
            tx,
            note.workspace_id,
            Some(note.session_id),
            "chat.absorbed",
            serde_json::json!({ "message_id": id, "absorbed_by": note_id }),
        )
        .await
        .map_err(|e| protocol(e.to_string()))?;
    }

    Ok(note_id)
}

/// The sleep a conversation is in, as a reader is shown it: which item ends it,
/// until when, and why. `None` when it is awake, which is nearly always.
///
/// One reading for both places a reader learns it -- the transcript read, for a
/// tab that was not open when the agent fell asleep, and the held event, for a
/// message sent while it sleeps -- so the two cannot disagree about what a
/// sleep is.
pub async fn asleep(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    session_id: Uuid,
) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let row: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select i.id, i.payload from action_items i \
           join inhibitors h on h.id = i.inhibitor_id \
          where i.workspace_id = $1 and i.state = 'pending' and i.kind = $3 \
            and h.level = 'session' and h.session_id = $2 \
          order by i.id desc limit 1",
    )
    .bind(workspace_id)
    .bind(session_id)
    .bind(SLEEP_ITEM)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id, payload)| {
        serde_json::json!({
            "item_id": id,
            "until": payload.get("until"),
            "reason": payload.get("reason"),
        })
    }))
}

// --------------------------------------------------------------------------
// Asking to wait: what the runtime relays when a guest calls `sleep`,
// `set-timer`, `list-timers` or `cancel-timer`.
// --------------------------------------------------------------------------

/// What the guest asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Asked {
    Sleep,
    Timer,
    List,
    Cancel,
}

#[derive(Debug, Deserialize)]
pub struct Wait {
    pub job_id: Uuid,
    pub session_id: Uuid,
    pub kind: Asked,
    #[serde(default)]
    pub seconds: Option<u64>,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub reason: String,
    /// The timer to cancel.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Arranged {
    /// What the model is told, as its tool result.
    pub message: String,
}

fn refuse(why: impl Into<String>) -> ApiError {
    (StatusCode::UNPROCESSABLE_ENTITY, why.into())
}

/// A time the agent named, read as a moment.
///
/// With an offset it is exact. Without one it is read in the zone of the person
/// the turn is for, which is what "remind me at 3pm" means -- and it spares the
/// model the offset arithmetic, which is where one set a timer for 21:12-07:00
/// having converted 14:12 to UTC and then put the local offset back on.
fn moment(at: &str, timezone: Option<&str>) -> Result<chrono::DateTime<chrono::Utc>, ApiError> {
    use chrono::TimeZone as _;
    let at = at.trim();
    if let Ok(exact) = chrono::DateTime::parse_from_rfc3339(at) {
        return Ok(exact.with_timezone(&chrono::Utc));
    }
    let naive = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|f| chrono::NaiveDateTime::parse_from_str(at, f).ok())
    .ok_or_else(|| {
        refuse("at must be a date and time, e.g. 2026-10-01T09:00:00 in the user's own zone")
    })?;
    let tz = timezone
        .and_then(|z| z.parse::<chrono_tz::Tz>().ok())
        .unwrap_or(chrono_tz::UTC);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => Ok(t.with_timezone(&chrono::Utc)),
        // The hour a clock goes back happens twice; the first is what a person
        // setting a reminder for it means.
        chrono::LocalResult::Ambiguous(first, _) => Ok(first.with_timezone(&chrono::Utc)),
        chrono::LocalResult::None => Err(refuse(format!(
            "{at} does not exist in {tz}: the clocks change then. Pick a time either side"
        ))),
    }
}

/// When a wait is due, or why it cannot be. The words are the model's to read,
/// so each refusal says what would be accepted.
fn due_at(
    input: &Wait,
    kind: Kind,
    now: chrono::DateTime<chrono::Utc>,
    timezone: Option<&str>,
) -> Result<chrono::DateTime<chrono::Utc>, ApiError> {
    let longest = chrono::Duration::from_std(LONGEST).expect("fits");
    let due = match (kind, input.seconds, input.at.as_deref()) {
        (_, Some(s), None) => {
            if s == 0 {
                return Err(refuse("seconds must be above zero"));
            }
            let s = i64::try_from(s).map_err(|_| refuse("that is longer than 30 days"))?;
            now + chrono::Duration::seconds(s.min(longest.num_seconds() + 1))
        }
        (Kind::Timer, None, Some(at)) => moment(at, timezone)?,
        (Kind::Sleep, None, Some(_)) => {
            return Err(refuse(
                "sleep takes seconds; use set_timer to wake at a time",
            ));
        }
        (_, Some(_), Some(_)) => return Err(refuse("give either at or seconds, not both")),
        (_, None, None) => return Err(refuse("give at or seconds")),
    };
    if due <= now {
        return Err(refuse("that time has already passed"));
    }
    if due - now > longest {
        return Err(refuse("that is longer than 30 days"));
    }
    Ok(due)
}

/// Who a sleep is shown to: the person whose conversation it is, or for one a
/// schedule or a webhook started, whoever set that up. `None` when there is
/// nobody -- the sleep is then refused rather than pausing a conversation nobody
/// can see is paused.
async fn owner_of(pool: &sqlx::PgPool, session_id: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select coalesce(s.user_id, sc.owner_id, w.owner_id) from agent_sessions s \
         left join schedules sc on sc.id = s.schedule_id \
         left join webhook_triggers w on w.id = s.webhook_trigger_id \
         where s.id = $1",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map(Option::flatten)
}

/// Arranges a sleep or a timer for the turn a runtime is running.
///
/// The runtime tier and the lease, as every report from a running turn needs,
/// and the conversation it names must be the one the turn is for: otherwise a
/// runtime holding one turn could pause another tenant's conversation.
pub async fn wait(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<Wait>,
) -> Result<Json<Arranged>, ApiError> {
    super::router::authorize(&state, &headers, Authority::WorkTake).await?;
    let job = jobs::get(&state.pool, input.job_id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "no such turn".to_string()))?;
    let lease = super::work::lease_from(&headers)?;
    if job.lease_token != Some(lease) {
        return Err((
            StatusCode::CONFLICT,
            "that turn is not leased to this runtime".to_string(),
        ));
    }
    let turn: super::worker::ChatTurnPayload = serde_json::from_value(job.payload.clone())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("payload: {e}")))?;
    if turn.session_id != input.session_id {
        return Err((
            StatusCode::FORBIDDEN,
            "that turn is not for that conversation".to_string(),
        ));
    }

    let kind = match input.kind {
        Asked::Sleep => Kind::Sleep,
        Asked::Timer => Kind::Timer,
        Asked::List => return list(&state.pool, &turn).await,
        Asked::Cancel => return cancel(&state.pool, &turn, input.id.as_deref()).await,
    };

    let reason = input.reason.trim().to_string();
    if reason.is_empty() {
        return Err(refuse("give a reason"));
    }
    if reason.chars().count() > LONGEST_REASON {
        return Err(refuse(format!(
            "keep the reason under {LONGEST_REASON} characters"
        )));
    }

    let now = chrono::Utc::now();
    let zone = turn.timezone.as_deref();
    let due_at = due_at(&input, kind, now, zone)?;
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);

    let mut due = Due {
        workspace_id: turn.workspace_id,
        session_id: turn.session_id,
        agent_id: turn.agent_id,
        kind,
        reason: reason.clone(),
        set_at: now,
        due_at,
        timezone: turn.timezone.clone(),
        item_id: None,
        inhibitor_id: None,
    };

    if kind == Kind::Sleep {
        // One sleep at a time. A second from the same turn would stack two
        // holds and two items for one pause.
        let asleep: bool = sqlx::query_scalar(
            "select exists (select 1 from inhibitors \
             where level = 'session' and session_id = $1 and held_by = $2)",
        )
        .bind(turn.session_id)
        .bind(SLEEP_HOLDER)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| internal(e.to_string()))?;
        if asleep {
            return Err(refuse("this conversation is already asleep"));
        }

        let Some(owner) = owner_of(&state.pool, turn.session_id)
            .await
            .map_err(|e| internal(e.to_string()))?
        else {
            return Err(refuse(
                "nobody could be told this conversation is asleep, so it cannot sleep; \
                 use set_timer instead",
            ));
        };

        // The same hold an approval takes, and for the same reason: what it
        // does is already built. Turns started while it stands park, and lifting
        // it gives them back.
        let inhibitors = super::inhibitor::PostgresInhibitorStore::new(state.pool.clone());
        let held = inhibitors
            .take(super::inhibitor::TakeInhibitor {
                scope: super::inhibitor::Scope::Session {
                    workspace_id: turn.workspace_id,
                    session_id: turn.session_id,
                },
                strength: super::inhibitor::Strength::Suspended,
                reason: format!("asleep until {}: {reason}", clock(due_at, zone)),
                held_by: SLEEP_HOLDER.to_string(),
            })
            .await?;

        let item = state
            .actions
            .raise(
                turn.workspace_id,
                super::actions::NewItem {
                    kind: SLEEP_ITEM.to_string(),
                    event_id: None,
                    inhibitor_id: Some(held.id),
                    payload: serde_json::json!({
                        "session_id": turn.session_id,
                        "agent_id": turn.agent_id,
                        "reason": reason,
                        "set_at": now,
                        "until": due_at,
                    }),
                    targets: vec![super::actions::Target::User(owner)],
                    // Expiring it would leave the hold with nothing to end it
                    // but the timer, which is what ends it anyway.
                    expires_at: None,
                },
            )
            .await;
        let item = match item {
            Ok(id) => id,
            Err(e) => {
                // Nothing would ever lift a hold whose item was never raised,
                // so the hold goes too rather than stranding the conversation.
                let _ = inhibitors.release(held.id).await;
                return Err(internal(e.to_string()));
            }
        };
        due.item_id = Some(item);
        due.inhibitor_id = Some(held.id);
    }

    let payload = serde_json::to_value(&due).map_err(|e| internal(e.to_string()))?;
    let delay = (due_at - now).to_std().unwrap_or_default();
    let id = jobs::enqueue(
        &state.pool,
        turn.workspace_id,
        WAKE,
        payload,
        Some(delay),
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .map_err(|e| internal(e.to_string()))?;

    if kind == Kind::Sleep {
        // Said to whoever has the conversation open, so the page can show it
        // paused now rather than when the first message parks.
        let _ = crate::events::append(
            &state.pool,
            turn.workspace_id,
            Some(turn.session_id),
            "chat.sleeping",
            serde_json::json!({
                "item_id": due.item_id,
                "until": due_at,
                "reason": reason,
            }),
        )
        .await;
    }

    // `when` and `in` are for the person reading the tool call as much as for
    // the model: the confirmation is where a timer set for the wrong day shows.
    let message = match kind {
        Kind::Sleep => serde_json::json!({
            "sleeping_until": due_at,
            "when": when(due_at, zone),
            "in": span((due_at - now).num_seconds()),
            "note": format!(
                "Asleep until {}. Your turn ends now. Anything sent meanwhile will be \
                 shown to you when you wake.",
                when(due_at, zone)
            ),
        }),
        Kind::Timer => {
            let others: Vec<serde_json::Value> = pending_timers(&state.pool, turn.session_id)
                .await
                .map_err(|e| internal(e.to_string()))?
                .into_iter()
                .filter(|(other, _)| *other != id)
                .map(|(other, due)| described(other, &due, now))
                .collect();
            let mut note = format!(
                "Timer {id} set for {}, {} from now. Carry on; you will be woken then. \
                 Check this is the time the user asked for.",
                when(due_at, zone),
                span((due_at - now).num_seconds())
            );
            if !others.is_empty() {
                note.push_str(&format!(
                    " {} other timer{} still pending in this conversation, listed in \
                     other_timers. Setting a timer never replaces one: cancel any that \
                     are no longer wanted with cancel_timer.",
                    others.len(),
                    if others.len() == 1 { " is" } else { "s are" },
                ));
            }
            serde_json::json!({
                "id": id,
                "timer_set_for": due_at,
                "when": when(due_at, zone),
                "in": span((due_at - now).num_seconds()),
                "reason": reason,
                "other_timers": others,
                "note": note,
            })
        }
    };
    Ok(Json(Arranged {
        message: message.to_string(),
    }))
}

/// The timers pending in a conversation, soonest first.
async fn pending_timers(
    pool: &sqlx::PgPool,
    session_id: Uuid,
) -> Result<Vec<(Uuid, Due)>, sqlx::Error> {
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select id, payload from jobs \
         where kind = $1 and state = 'pending' \
           and payload->>'session_id' = $2 and payload->>'kind' = 'timer' \
         order by run_after, id",
    )
    .bind(WAKE)
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, payload)| Some((id, serde_json::from_value(payload).ok()?)))
        .collect())
}

/// One timer as a tool result shows it.
fn described(id: Uuid, due: &Due, now: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "at": due.due_at,
        "when": when(due.due_at, due.timezone.as_deref()),
        "in": span((due.due_at - now).num_seconds()),
        "reason": due.reason,
    })
}

/// `list-timers`: what is pending here, so an agent can say what reminders
/// exist rather than recalling what it believes it set.
async fn list(
    pool: &sqlx::PgPool,
    turn: &super::worker::ChatTurnPayload,
) -> Result<Json<Arranged>, ApiError> {
    let now = chrono::Utc::now();
    let timers: Vec<serde_json::Value> = pending_timers(pool, turn.session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .iter()
        .map(|(id, due)| described(*id, due, now))
        .collect();
    let note = match timers.len() {
        0 => "No timers are pending in this conversation.".to_string(),
        1 => "1 timer is pending in this conversation.".to_string(),
        n => format!("{n} timers are pending in this conversation."),
    };
    Ok(Json(Arranged {
        message: serde_json::json!({ "timers": timers, "note": note }).to_string(),
    }))
}

/// `cancel-timer`: removes one, scoped to this conversation so an id from
/// anywhere else is simply not found.
async fn cancel(
    pool: &sqlx::PgPool,
    turn: &super::worker::ChatTurnPayload,
    id: Option<&str>,
) -> Result<Json<Arranged>, ApiError> {
    let not_found = || {
        refuse(
            "no timer with that id is pending in this conversation; list_timers shows the ones that are",
        )
    };
    let id: Uuid = id
        .and_then(|id| id.trim().parse().ok())
        .ok_or_else(not_found)?;
    // Only a pending one: a timer already firing is past cancelling, and its
    // note is on its way.
    let removed: Option<serde_json::Value> = sqlx::query_scalar(
        "delete from jobs \
         where id = $1 and kind = $2 and state = 'pending' \
           and payload->>'session_id' = $3 and payload->>'kind' = 'timer' \
         returning payload",
    )
    .bind(id)
    .bind(WAKE)
    .bind(turn.session_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let due: Due = removed
        .and_then(|p| serde_json::from_value(p).ok())
        .ok_or_else(not_found)?;
    let cancelled = described(id, &due, chrono::Utc::now());
    Ok(Json(Arranged {
        message: serde_json::json!({
            "cancelled": cancelled,
            "note": format!("Cancelled the timer for {}.", when(due.due_at, due.timezone.as_deref())),
        })
        .to_string(),
    }))
}

// --------------------------------------------------------------------------
// Waking early.
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Woken {
    /// Whether this call ended the sleep. False when it had already ended --
    /// two people pressing Wake now, or the timer arriving first.
    pub woke: bool,
}

/// Ends a conversation's sleep now.
///
/// For anybody who may send to the conversation, since sending is what the
/// sleep holds back: somebody allowed to say something to the agent is allowed
/// to wake it to hear it.
pub async fn wake_now(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Woken>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    let _session = super::sessions::session_for(
        &state,
        &claims,
        session_id,
        Authority::SessionsCreate,
        super::sessions::Ownership::Insufficient,
    )
    .await?;

    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let pending = sqlx::query(
        "select j.id, j.payload from jobs j \
         where j.kind = $1 and j.state = 'pending' and j.workspace_id = $2 \
           and j.payload->>'session_id' = $3 and j.payload->>'kind' = 'sleep' \
         order by j.id desc limit 1",
    )
    .bind(WAKE)
    .bind(claims.workspace_id)
    .bind(session_id.to_string())
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| internal(e.to_string()))?;
    let Some(pending) = pending else {
        return Ok(Json(Woken { woke: false }));
    };
    let due: Due =
        serde_json::from_value(pending.get("payload")).map_err(|e| internal(e.to_string()))?;

    let name = state
        .users
        .get(claims.subject)
        .await
        .ok()
        .map(|u| u.display_name);

    let woke = settle(
        state.actions.as_ref(),
        &due,
        Some(claims.subject),
        Some(name.unwrap_or_else(|| "Somebody".to_string())),
    )
    .await
    .map_err(internal)?;
    Ok(Json(Woken { woke }))
}

/// Ends a sleep: settles its item, writes the note, lifts the hold and gives
/// back what parked, all in the one transaction `settle_and_release` owns.
///
/// Returns false when the sleep had already ended, which is not an error: of
/// the two things that can end one, the second finds nothing left to do.
async fn settle(
    actions: &dyn super::actions::ActionStore,
    due: &Due,
    resolved_by: Option<Uuid>,
    woken_by: Option<String>,
) -> Result<bool, String> {
    let (Some(item_id), Some(hold)) = (due.item_id, due.inhibitor_id) else {
        return Err("a sleep without its item or hold".into());
    };
    let outcome = actions
        .settle_and_release(super::actions::Settle {
            workspace_id: due.workspace_id,
            item_id,
            state: super::actions::State::Resolved,
            resolved_by,
            note: Some(if woken_by.is_some() {
                "woken early"
            } else {
                "slept until it was due"
            }),
            hold: Some(hold),
            grant: None,
            wake: Some(Note::of(due, woken_by)),
        })
        .await;
    match outcome {
        Ok(_) => Ok(true),
        Err(super::actions::ActionError::NotPending(_) | super::actions::ActionError::NotFound) => {
            Ok(false)
        }
        Err(e) => Err(e.to_string()),
    }
}

// --------------------------------------------------------------------------
// The wakeups coming due.
// --------------------------------------------------------------------------

/// Fires wakeups as they come due, in the API process like the other
/// background work: it needs the database and nothing else.
pub fn spawn(
    pool: sqlx::PgPool,
    actions: Arc<dyn super::actions::ActionStore>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = shutdown.notified() => return,
                _ = ticker.tick() => {
                    let claimed = match jobs::claim(&pool, &[WAKE], 10, jobs::DEFAULT_LEASE).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(error = %e, "could not claim wakeups");
                            continue;
                        }
                    };
                    for handle in claimed {
                        let job = handle.job;
                        let done = match fire(&pool, actions.as_ref(), &job).await {
                            Ok(()) => jobs::complete(&pool, job.id, job.lease_token).await,
                            Err(e) => {
                                tracing::warn!(job_id = %job.id, error = %e, "a wakeup failed");
                                jobs::fail(&pool, job.id, &e, Duration::from_secs(10), job.lease_token).await
                            }
                        };
                        if let Err(e) = done {
                            tracing::warn!(job_id = %job.id, error = %e, "could not close a wakeup");
                        }
                    }
                }
            }
        }
    });
}

/// One wakeup, due now.
pub async fn fire(
    pool: &sqlx::PgPool,
    actions: &dyn super::actions::ActionStore,
    job: &jobs::Job,
) -> Result<(), String> {
    let due: Due = serde_json::from_value(job.payload.clone()).map_err(|e| e.to_string())?;
    match due.kind {
        Kind::Sleep => {
            let woke = settle(actions, &due, None, None).await?;
            if !woke {
                tracing::debug!(session_id = %due.session_id, "a sleep had already been ended");
            }
            Ok(())
        }
        Kind::Timer => {
            // A conversation deleted while the timer waited is nothing to wake.
            let exists: bool =
                sqlx::query_scalar("select exists (select 1 from agent_sessions where id = $1)")
                    .bind(due.session_id)
                    .fetch_one(pool)
                    .await
                    .map_err(|e| e.to_string())?;
            if !exists {
                return Ok(());
            }
            let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
            write_tx(&mut tx, Note::of(&due, None))
                .await
                .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
    }

    fn note(kind: Kind) -> Note {
        Note {
            workspace_id: Uuid::nil(),
            session_id: Uuid::nil(),
            agent_id: Uuid::nil(),
            kind,
            reason: "Waiting for the deposit to clear".into(),
            set_at: at(0),
            due_at: at(600),
            timezone: None,
            woken_by: None,
        }
    }

    #[test]
    fn a_span_reads_as_a_person_would_say_it() {
        assert_eq!(span(1), "1 second");
        assert_eq!(span(45), "45 seconds");
        assert_eq!(span(60), "1 minute");
        assert_eq!(span(480), "8 minutes");
        assert_eq!(span(7200), "2 hours");
        assert_eq!(span(3 * 86_400), "3 days");
    }

    #[test]
    fn a_sleep_with_nothing_waiting_says_how_long_and_why() {
        let text = compose(&note(Kind::Sleep), at(600), &[]);
        assert!(text.contains("(10 minutes)"), "{text}");
        assert!(text.contains("Waiting for the deposit to clear."), "{text}");
        assert!(!text.contains("arrived"), "{text}");
    }

    /// The case this exists for: messages sent while the agent slept, each
    /// timed against the wake so the words stay true however late they are read.
    #[test]
    fn a_sleep_names_what_arrived_and_when_relative_to_the_wake() {
        let text = compose(&note(Kind::Sleep), at(600), &[at(120), at(300), at(540)]);
        assert!(text.contains("3 messages arrived"), "{text}");
        assert!(
            text.contains("8 minutes, 5 minutes and 1 minute before you woke"),
            "{text}"
        );
        assert!(text.contains("Answer them together"), "{text}");
    }

    #[test]
    fn one_message_is_spoken_of_as_one() {
        let text = compose(&note(Kind::Sleep), at(600), &[at(540)]);
        assert!(text.contains("1 message arrived"), "{text}");
        assert!(text.contains("Answer it."), "{text}");
    }

    #[test]
    fn a_sleep_ended_early_says_who_and_what_was_asked() {
        let mut n = note(Kind::Sleep);
        n.woken_by = Some("Ben".into());
        let text = compose(&n, at(120), &[]);
        assert!(text.contains("Ben woke you early"), "{text}");
        assert!(text.contains("(2 minutes)"), "{text}");
    }

    #[test]
    fn a_timer_says_it_fired_and_why() {
        let text = compose(&note(Kind::Timer), at(600), &[]);
        assert!(text.starts_with("A timer you set at"), "{text}");
        assert!(text.contains("Waiting for the deposit to clear."), "{text}");
    }

    #[test]
    fn the_time_is_read_in_the_users_zone() {
        let mut n = note(Kind::Sleep);
        n.timezone = Some("Australia/Brisbane".into());
        let text = compose(&n, at(600), &[]);
        assert!(text.contains("AEST"), "{text}");
    }

    fn wait(kind: Kind, seconds: Option<u64>, at: Option<&str>) -> (Wait, Kind) {
        let wait = Wait {
            job_id: Uuid::nil(),
            session_id: Uuid::nil(),
            kind: match kind {
                Kind::Sleep => Asked::Sleep,
                Kind::Timer => Asked::Timer,
            },
            seconds,
            at: at.map(str::to_string),
            reason: "x".into(),
            id: None,
        };
        (wait, kind)
    }

    fn due(
        (wait, kind): (Wait, Kind),
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<chrono::DateTime<chrono::Utc>, ApiError> {
        due_at(&wait, kind, now, None)
    }

    #[test]
    fn a_wait_must_be_in_the_future_and_within_thirty_days() {
        let now = at(0);
        assert!(due(wait(Kind::Sleep, Some(60), None), now).is_ok());
        assert!(due(wait(Kind::Sleep, Some(0), None), now).is_err());
        assert!(due(wait(Kind::Sleep, Some(31 * 86_400), None), now).is_err());
        assert!(due(wait(Kind::Timer, None, Some("2020-01-01T00:00:00Z")), now).is_err());
    }

    #[test]
    fn a_sleep_takes_seconds_and_a_timer_takes_either() {
        let now = at(0);
        let later = (at(3600)).to_rfc3339();
        assert!(due(wait(Kind::Sleep, None, Some(&later)), now).is_err());
        assert_eq!(
            due(wait(Kind::Timer, None, Some(&later)), now).unwrap(),
            at(3600)
        );
        assert!(due(wait(Kind::Timer, Some(60), Some(&later)), now).is_err());
        assert!(due(wait(Kind::Timer, None, None), now).is_err());
    }

    /// The case that set a timer seven hours out: a time with no offset is the
    /// user's own clock, and the model need not do the arithmetic.
    #[test]
    fn a_time_without_an_offset_is_read_in_the_users_zone() {
        let read = moment("2026-09-29T15:12", Some("America/Los_Angeles")).unwrap();
        assert_eq!(read.to_rfc3339(), "2026-09-29T22:12:00+00:00");
        let exact = moment("2026-09-29T15:12:00-07:00", Some("Australia/Brisbane")).unwrap();
        assert_eq!(exact, read, "an offset given is an offset kept");
        assert!(moment("at three", None).is_err());
    }

    #[test]
    fn a_time_the_clocks_skip_is_refused_and_one_they_repeat_is_the_first() {
        // Spring forward in Los Angeles: 02:30 on 8 March 2026 never happens.
        assert!(moment("2026-03-08T02:30", Some("America/Los_Angeles")).is_err());
        // Fall back: 01:30 on 1 November 2026 happens twice; the first is PDT.
        let first = moment("2026-11-01T01:30", Some("America/Los_Angeles")).unwrap();
        assert_eq!(first.to_rfc3339(), "2026-11-01T08:30:00+00:00");
    }

    #[test]
    fn a_confirmation_names_the_day_and_the_zone() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-09-29T22:12:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            when(at, Some("America/Los_Angeles")),
            "Tuesday 29 September at 3:12 pm PDT"
        );
        assert_eq!(when(at, None), "Tuesday 29 September at 10:12 pm UTC");
    }
}
