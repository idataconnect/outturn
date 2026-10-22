-- no-transaction
-- A conversation's pending wakes, by the conversation: listing its timers,
-- cancelling one, and waking it early (`wake.rs`). The expression indexes on
-- the payload's session are partial on `chat.turn`, so these read every
-- pending job of every kind. The queries cast the session to uuid and write
-- the kind and state in, which is what lets the planner prove this predicate.
--
-- Concurrently, alone and without a transaction: `jobs` is written on every
-- claim, heartbeat and finish, and a plain build would hold all of them off.
-- A build that fails leaves an invalid index under this name and the migration
-- then fails on it; drop it concurrently and start the API again.
create index concurrently jobs_wake_session_idx
    on jobs (((payload->>'session_id')::uuid), id)
    where kind = 'chat.wake' and state = 'pending';
