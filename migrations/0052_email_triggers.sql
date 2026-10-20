-- Email as a third kind of trigger: mail delivered by an inbound service to a
-- hook's path, parsed here (docs/triggers.md, "Email"). It shares the hooks'
-- path space, so the path is unique across both.
alter table triggers drop constraint triggers_kind_check;
alter table triggers add constraint triggers_kind_check
    check (kind in ('schedule', 'webhook', 'email'));

drop index triggers_webhook_path_idx;
create unique index triggers_hook_path_idx on triggers ((config->>'path'))
    where kind in ('webhook', 'email');

alter table agent_sessions drop constraint agent_sessions_started_by_check;
alter table agent_sessions add constraint agent_sessions_started_by_check
    check (started_by in ('schedule', 'webhook', 'email'));
