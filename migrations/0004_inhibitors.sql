-- Holds on work: a spend cap tripping, an operator stopping a runaway agent, a
-- turn waiting for somebody to approve a tool call.
--
-- A set rather than a flag on the workspace, because a flag cannot say who is
-- holding it. Two reasons to stop the same org -- a cap and an abuse
-- investigation -- would share one bit, and whoever clears theirs first clears
-- both. So each hold is a row with a holder, and work proceeds when no row
-- applies. See docs/inhibitors.md.
--
-- Scope is columns rather than a single nullable id because the levels are not
-- interchangeable: a platform hold has no workspace, a session hold has both a
-- workspace and a session, and the cascade reads them by name.
create table inhibitors (
    id           uuid primary key,

    -- 'platform', 'workspace', 'agent' or 'session'. The level decides which
    -- of the columns below are set, which the check constraint enforces rather
    -- than trusting the writer.
    level        text        not null check (level in ('platform', 'workspace', 'agent', 'session')),
    workspace_id uuid        references workspaces (id) on delete cascade,
    agent_id     uuid,
    session_id   uuid,

    -- 'suspended' may continue later; 'stopped' ends the turn. The join takes
    -- the strongest that applies, so this is an ordering and not a flag.
    strength     text        not null check (strength in ('suspended', 'stopped')),

    -- Why, in a person's words, and what took it. Shown wherever the hold is
    -- shown: "why is nothing happening" is the question this table answers.
    -- `held_by` is text because a spend cap and a person are both legitimate
    -- holders and only one of them has a row in users.
    reason       text        not null,
    held_by      text        not null,

    created_at   timestamptz not null default now(),

    -- A level's columns are exactly the ones it needs. Anything else is a hold
    -- that covers something other than what its writer meant.
    check (
        (level = 'platform'  and workspace_id is null and agent_id is null and session_id is null)
     or (level = 'workspace' and workspace_id is not null and agent_id is null and session_id is null)
     or (level = 'agent'     and workspace_id is not null and agent_id is not null and session_id is null)
     or (level = 'session'   and workspace_id is not null and session_id is not null and agent_id is null)
    )
);

-- The cascade, which runs on every turn preparation: everything covering one
-- session, across the levels above it.
create index inhibitors_covering_idx on inhibitors (workspace_id, level);
-- Platform holds have no workspace, so they are not in the index above.
create index inhibitors_platform_idx on inhibitors (level) where level = 'platform';
create index inhibitors_agent_idx on inhibitors (agent_id) where agent_id is not null;
create index inhibitors_session_idx on inhibitors (session_id) where session_id is not null;

-- Stopping latches, and this is where it is recorded.
--
-- Separate from the inhibitor that caused it because they outlive each other:
-- flipping the kill switch back off releases the hold and must not resume
-- anything. A stopped conversation stays stopped until a person says something,
-- which is what makes it a kill switch rather than a pause with a harsher name.
--
-- Cleared by a message carrying a real user_id -- see agent_messages.user_id,
-- which is null for anything the platform produced. The agent cannot clear its
-- own latch, and neither can a steer it provoked.
alter table agent_sessions
    add column stopped_at timestamptz,
    -- What to tell the next turn, so it does not read its own reply trailing
    -- off mid-sentence and apologise for something it did not do.
    add column stopped_reason text;
