-- A second pass over queries whose cost grew with history rather than with
-- what they were asked: skill stats, building a turn, and a workspace's user
-- list. Each section says what read too much.

-- Skill stats answered questions about a window by reading every turn that
-- ever used a skill.
--
-- The smallest UUIDv7 minted at or after `at`. A v7 id leads with its
-- millisecond timestamp, so "ids minted in this window" is a range of the
-- key -- which is how a window over `turn_skills`, keyed on the reply's id,
-- becomes a range scan rather than a read of all of it. Immutable: the
-- answer depends on nothing but its argument.
create function uuid7_floor(at timestamptz) returns uuid
language sql immutable as $$
    select (substr(h, 1, 8) || '-' || substr(h, 9, 4) || '-0000-0000-000000000000')::uuid
      from (select lpad(to_hex(floor(extract(epoch from at) * 1000)::bigint), 12, '0') as h) t
$$;

-- A skill's turns, newest first: its last use is the first entry, and a
-- window is a range within it. Also what a skill's deletion cascades
-- through, which without it scanned every turn ever recorded.
create index turn_skills_skill_idx on turn_skills (skill_id, reply_id desc);

-- Versions written in a window, per workspace: the panel's counts and its
-- authors.
create index skill_versions_workspace_created_idx
    on skill_versions (workspace_id, created_at);

-- Not skills, but found by the same sweep: building a turn read its session's
-- whole history, though the projection keeps only the newest summary and what
-- comes after it. The read now starts there, and finding the newest summary
-- is this rather than a walk of every message in the session.
create index agent_messages_summary_idx
    on agent_messages (session_id, id)
    where metadata ? 'summary_through';

-- A workspace's members in id order, which is how its user list pages. The
-- workspace index alone cannot produce that order, so the list was driven
-- from `users` instead and could walk the whole platform to fill one page.
-- Replaces the bare workspace index, whose lookups it also serves.
drop index if exists user_workspace_roles_workspace_idx;
create index user_workspace_roles_member_idx
    on user_workspace_roles (workspace_id, user_id);

-- The platform-wide usage view, for a system administrator: every workspace's
-- ledger over a window. Every ledger index leads with the workspace, so this
-- read whole partitions. BRIN, because the ledger is appended in time order
-- and a block-range index of it is a few pages however long it grows.
create index usage_ledger_occurred_brin on usage_ledger using brin (occurred_at);
