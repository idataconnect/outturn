-- no-transaction
-- The streamed-event sweep, which replaces the delta sweep: every kind a turn
-- streams while it runs, not only `chat.delta`, once the turn has finished
-- (`chat::forget_streamed`). The predicate is the sweep's own, word for word,
-- so the planner can prove the query implies it.
--
-- Concurrently, and so alone in a migration with no transaction around it:
-- `events` takes a write per streamed token, and a plain build holds a SHARE
-- lock that stalls every turn streaming for as long as it takes, which 0024
-- did. A concurrent build that fails leaves an invalid index behind under
-- this name, and this migration then fails again on it rather than passing
-- over it; `drop index concurrently events_stream_sweep_idx` and start the
-- API again.
create index concurrently events_stream_sweep_idx
    on events (created_at)
    where kind in ('chat.delta', 'chat.reasoning', 'chat.writing', 'chat.tool',
                   'chat.tool_result', 'chat.steer');
