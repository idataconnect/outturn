-- Indexes for queries that ran often and read history to answer a question
-- about the present. Each was found by EXPLAIN against seeded volume, not by
-- reading; the figures are from that run and are kept so the next person can
-- tell what the index was for if the query changes shape.

-- `grant::was_answered`, asked as every turn is prepared: has this job's
-- approval been answered? Every other `job_id` index on action_items is
-- partial on `pending`, which is exactly the rows this does not want, so it
-- read the workspace's whole approval history. 431ms at 200k items.
--
-- The predicate is the query's own, word for word: a partial index is only
-- used when the planner can prove the query implies it.
create index action_items_answered_job_idx
    on action_items (workspace_id, (payload->>'job_id'))
    where kind like 'approval.%' and state in ('resolved', 'cancelled');

-- `absorbed_by` references agent_messages with `on delete set null`, and an
-- unindexed foreign key makes every delete of a message scan the table for
-- rows pointing at it. `discard_placeholder` deletes one per turn, and a
-- session delete cascades through all of its messages: 1.7s for a session of
-- a hundred against 200k messages, 2.9ms with this. Partial because almost
-- every row is null.
create index agent_messages_absorbed_by_idx
    on agent_messages (absorbed_by)
    where absorbed_by is not null;

-- The worker's delta sweep, every reaper tick on every API replica: deltas
-- older than a day. Nothing indexed `kind` or `created_at`, so it read every
-- event ever written to find the old deltas -- 322ms at a million events,
-- whether or not there was anything to delete.
create index events_delta_sweep_idx
    on events (created_at)
    where kind = 'chat.delta';
