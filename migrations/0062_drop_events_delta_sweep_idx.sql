-- no-transaction
-- `events_delta_sweep_idx` (0024) served the delta-only sweep, and every row
-- it holds is also in `events_stream_sweep_idx`. Concurrently, for the reason
-- the index before this one was built that way.
drop index concurrently if exists events_delta_sweep_idx;
