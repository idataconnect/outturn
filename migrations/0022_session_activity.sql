-- The recent list is by last activity, not by creation: a conversation
-- somebody picked up again this morning belongs at the top however old it is.
-- See docs/session-search.md.
--
-- Written by a trigger on `agent_messages` rather than by each statement that
-- stores a message. There are five of those -- chat, triggers, wakes, replies
-- and their placeholders -- and a sixth added later would leave its sessions
-- sinking down the list with nothing to say why. The trigger runs in the
-- storing statement's own transaction, so it cannot drift from it.
alter table agent_sessions add column last_active_at timestamptz;

update agent_sessions s
   set last_active_at = coalesce(
       (select max(m.created_at) from agent_messages m where m.session_id = s.id),
       s.created_at);

alter table agent_sessions
    alter column last_active_at set not null,
    alter column last_active_at set default now();

create function touch_session() returns trigger language plpgsql as $$
begin
    update agent_sessions set last_active_at = now() where id = new.session_id;
    return null;
end
$$;

-- On update too: a reply is created empty when its turn starts and written
-- when it finishes, and finishing is activity.
create trigger agent_messages_touch_session
    after insert or update of content on agent_messages
    for each row execute function touch_session();

-- The id breaks ties: timestamps collide, and a keyset cursor over a column
-- that collides skips rows. Nothing else read the old order.
drop index if exists agent_sessions_workspace_idx;
create index agent_sessions_recent_idx
    on agent_sessions (workspace_id, last_active_at desc, id desc);
