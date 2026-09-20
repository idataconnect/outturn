-- Turns that start because the clock said so.
--
-- A trigger is work nobody is watching, which is the whole of why the columns
-- below look the way they do: there is no person to notice it firing wrong, to
-- read the reply, or to say who set it running.
create table schedules (
    id           uuid        primary key,
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    agent_id     uuid        not null references agents (id) on delete cascade,

    name         text        not null,

    -- The message the turn begins with. Stored as an ordinary user message
    -- when it fires, but with no `user_id` -- it is the schedule's words and
    -- not a person's, and a reader who opens the session should see that.
    prompt       text        not null,

    -- Standard five-field cron. The storage format rather than the interface:
    -- an expression is exact and nobody reads one correctly, so the editor
    -- offers the ordinary shapes and shows the next few firings as dates.
    expression   text        not null,

    -- IANA name, e.g. "Europe/London". Not an offset: "every weekday at 9"
    -- means nine where somebody is, and an offset stops meaning that twice a
    -- year. The turn's own clock already works this way -- `current-time`
    -- takes the zone from the turn rather than the machine.
    timezone     text        not null default 'UTC',

    enabled      boolean     not null default true,

    -- Who set this up, kept apart from who is waiting for the reply, because
    -- those are different questions and a triggered turn has no answer to the
    -- second. The turn's `user_id` stays null: it is what the usage ledger
    -- expects of work nobody sent, and it is what stops an agent clearing its
    -- own session's stopped latch, since `worker::inhibited` lifts that only
    -- for a prompt carrying a real user.
    --
    -- Null when the person who made it has been deleted. The schedule keeps
    -- running: stopping somebody's morning report because they left is a
    -- surprise in the dangerous direction, and `enabled` is how a schedule is
    -- turned off on purpose.
    owner_id     uuid        references users (id) on delete set null,

    -- When this should next fire, and the only thing the firing loop reads to
    -- decide. Written forward as each firing happens rather than computed as a
    -- series: a series drifts the moment the expression is edited and needs a
    -- sweep to correct, where a successor written at firing time has no series
    -- to correct and a disabled schedule simply stops writing one.
    next_run_at  timestamptz,

    -- What happened last time, for the row in the list. Until notifications
    -- exist this is the only place anybody learns a schedule has been failing
    -- every morning for a week.
    last_run_at  timestamptz,
    last_status  text        check (last_status in ('ok', 'failed', 'skipped')),
    last_error   text,

    -- How many firings were passed over because their time was already gone --
    -- a deployment that was down overnight should not wake to twenty-four
    -- queued turns. The count is kept rather than the skip being silent,
    -- because a schedule that quietly missed a week looks exactly like one
    -- that never worked.
    skipped      int         not null default 0,

    created_by   uuid        references users (id) on delete set null,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

-- The firing loop's only query: enabled schedules whose time has come.
create index schedules_due_idx on schedules (next_run_at)
    where enabled and next_run_at is not null;

create index schedules_agent_idx on schedules (workspace_id, agent_id);

-- Which schedule started a session, where one did.
--
-- Set null rather than cascading: deleting a schedule should not delete the
-- transcripts of what it did, which are the record of work that actually
-- happened and may be the only account of it.
alter table agent_sessions
    add column schedule_id uuid references schedules (id) on delete set null;
