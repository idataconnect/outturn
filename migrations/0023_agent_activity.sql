-- What the dashboard's "right now" panel reads: per agent, its turns in flight
-- and when it was last active. Both are answered from indexes sized by what
-- is happening rather than by what has ever happened, so the panel costs the
-- same in a workspace's fifth year as in its first.

-- Live turns by workspace. Partial on the live states, like
-- `jobs_chat_turn_session_idx`, but keyed by workspace: that one is keyed by
-- session, so a workspace-wide question would read every live turn in the
-- cluster. This holds the handful in flight and nothing else.
create index jobs_live_turn_workspace_idx
    on jobs (workspace_id)
    where kind = 'chat.turn' and state in ('pending', 'running', 'parked');

-- An agent's most recent session, as one probe: "latest" is not something a
-- partial index can say, but an index ordered by it answers `limit 1` from its
-- first entry. Replaces the bare agent index, whose lookups it also serves.
drop index if exists agent_sessions_agent_idx;
create index agent_sessions_agent_recent_idx
    on agent_sessions (agent_id, last_active_at desc);
