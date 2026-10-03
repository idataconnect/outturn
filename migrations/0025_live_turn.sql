-- The state of a session's live turn, written once.
--
-- Two reads of a session asked this by two copies of the same subquery, and
-- both took the newest live job -- which, when a follow-up is queued behind a
-- turn still streaming, is the follow-up: the list said "queued" over a reply
-- visibly being written. Running comes first, the same order and for the same
-- reason as the stop button's lookup in `jobs::store`.
--
-- Its state list is one of the places AGENTS.md means by "a job state is
-- enumerated in more places than the schema". So is the predicate of
-- `jobs_live_turn_workspace_idx`, which must stay word for word what
-- `sessions::agent_activity` filters on, or the planner stops using it.
create function live_turn(session uuid) returns text
language sql stable as $$
    select state from jobs
     where kind = 'chat.turn'
       and state in ('pending', 'running', 'parked')
       and (payload->>'session_id')::uuid = session
     order by case state when 'running' then 0 when 'pending' then 1 else 2 end, id
     limit 1
$$;

-- Deleting a message is activity too. A turn stopped before its first token
-- discards its empty reply rather than writing it, and with the trigger
-- firing only on insert and update that ending touched nothing: the session
-- kept its place, and a list refreshing only its most recent page went on
-- showing the turn as live.
create or replace function touch_session() returns trigger language plpgsql as $$
begin
    update agent_sessions set last_active_at = now()
     where id = case tg_op when 'DELETE' then old.session_id else new.session_id end;
    return null;
end
$$;

drop trigger agent_messages_touch_session on agent_messages;
create trigger agent_messages_touch_session
    after insert or update of content or delete on agent_messages
    for each row execute function touch_session();
