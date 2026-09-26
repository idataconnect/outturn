-- Parking a turn that is waiting on a person.
--
-- A suspended inhibitor means "pause here and carry on later" -- see
-- docs/inhibitors.md -- and until now there was nothing for "later" to pick up:
-- the suspended verdict declined the turn and completed the job, so the
-- announcement promised a resumption that could never happen.
--
-- `parked` is that missing state. A parked job has run, been refused, and is
-- waiting for the hold to lift -- which is none of the other four. It is not
-- `pending`, because nothing should claim it until somebody releases the hold;
-- not `running`, because no runtime holds it and no lease is being renewed; and
-- not an outcome, because the turn has not finished. The same distinction the
-- comment on `cancel_requested_at` draws: a request is not an outcome.
--
-- Deliberately a state rather than a far-future `run_after`, which was the
-- other option. That would have needed no migration and no new value, at the
-- cost of making a turn waiting on a human indistinguishable from one merely
-- scheduled -- in the one tier that owns the answer, with
-- `jobs_claimable_idx` filling with rows nothing will claim and `job_backlog`
-- counting work that cannot start. The convention would still have had to be
-- honoured everywhere the state would have been; a state gets a constraint to
-- catch mistakes.
alter table jobs drop constraint jobs_state_check;
alter table jobs add constraint jobs_state_check
    check (state in ('pending', 'running', 'succeeded', 'failed', 'cancelled',
                     'parked'));

-- Resuming is a lookup by session: a hold is released against a conversation,
-- and what has to be found is the turn parked under it.
--
-- Partial on the state, like `jobs_chat_turn_session_idx` below, and for the
-- same reason: parked rows are a handful at a time, against a table that holds
-- every turn ever queued.
create index jobs_parked_session_idx
    on jobs (((payload->>'session_id')::uuid), id)
    where state = 'parked';

-- The stop button's lookup now includes parked, so its index has to as well.
--
-- Recreated rather than added beside: the predicate is what makes the index
-- small, and a second one on the same expression would be two indexes to keep
-- and two for the planner to choose between. `if exists` because a database
-- restored from before this migration may not have it under that name.
drop index if exists jobs_chat_turn_session_idx;
create index jobs_chat_turn_session_idx
    on jobs (((payload->>'session_id')::uuid), id)
    where kind = 'chat.turn' and state in ('pending', 'running', 'parked');

-- `job_backlog` is deliberately left alone.
--
-- It answers "how much work could start right now", which is what the
-- autoscaler reads, and a parked turn could not: it is waiting on a person and
-- no pod can advance it. Counting one would ask for capacity to run work that
-- will not run, and a workspace with a dozen pending approvals would hold pods
-- open overnight for turns nobody can take. The view filters on `state =
-- 'pending'`, so parked rows fall out of it without a change -- which is the
-- reason the state exists rather than a far-future `run_after`, since a parked
-- row that stayed pending *would* have been counted.
