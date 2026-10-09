-- What started a session, kept after the thing that started it is gone.
--
-- `schedule_id` and `webhook_trigger_id` are set null when their trigger is
-- deleted, which is right for a link and wrong for a record: a conversation a
-- schedule started was still started by a schedule, and without this column
-- it would read afterwards as though a person had opened it. Null is a person,
-- or the platform; the two kinds are the triggers that exist.
alter table agent_sessions
    add column started_by text check (started_by in ('schedule', 'webhook'));

update agent_sessions set started_by = 'schedule' where schedule_id is not null;
update agent_sessions set started_by = 'webhook' where webhook_trigger_id is not null;
