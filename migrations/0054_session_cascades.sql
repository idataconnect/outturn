-- What a conversation owns goes with it.
--
-- `delete from agent_sessions` relied on cascades, and three tables that
-- carry a session id had none: the events it was told through, the holds
-- taken on it, and the approvals granted within it. Deleting a session left
-- all three behind, keyed on an id nothing would ever look up again -- and a
-- session hold left behind is a row the cascade of every later turn
-- preparation still reads.
--
-- Rows already orphaned by earlier deletes go first, or the constraints
-- cannot be added. They describe conversations that no longer exist, and
-- nothing could have reached them since.
--
-- Jobs are not here. A job's session id is inside its payload, which a
-- foreign key cannot reach, so the delete path cancels them in its own
-- transaction. Action items are not here either: an item is settled rather
-- than deleted so a queue that emptied can still say why, and cascading
-- items from their hold would take the record of every settled approval
-- with the hold its settling lifts. The delete path cancels a session's
-- pending items the way a person would.

delete from events e
 where e.session_id is not null
   and not exists (select 1 from agent_sessions s where s.id = e.session_id);

delete from inhibitors i
 where i.session_id is not null
   and not exists (select 1 from agent_sessions s where s.id = i.session_id);

delete from approval_grants g
 where not exists (select 1 from agent_sessions s where s.id = g.session_id);

alter table events
    add constraint events_session_id_fkey
    foreign key (session_id) references agent_sessions (id) on delete cascade;

alter table inhibitors
    add constraint inhibitors_session_id_fkey
    foreign key (session_id) references agent_sessions (id) on delete cascade;

alter table approval_grants
    add constraint approval_grants_session_id_fkey
    foreign key (session_id) references agent_sessions (id) on delete cascade;
