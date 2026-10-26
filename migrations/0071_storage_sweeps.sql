-- Session files were swept by a bucket lifecycle rule, which can only count
-- an object's own age: a conversation older than the window lost its uploads
-- mid-conversation, however lively it was. The API sweeps them instead, from
-- the session's last activity, which only the database knows.
--
-- That leaves what a lifecycle rule did for nothing in particular: the files
-- of a session, agent or workspace that was deleted, whose row is no longer
-- there to say when it was last active. A trigger writes down what to remove
-- as the row goes -- a trigger rather than the deleting statements, so a
-- cascade is caught too -- and the same sweep empties those prefixes. Agent
-- and workspace files were never swept at all before this.

create table storage_deletions (
    prefix       text        primary key,
    requested_at timestamptz not null default now()
);

alter table agent_sessions add column files_swept_at timestamptz;

create function storage_deletion_requested() returns trigger language plpgsql as $$
begin
    if tg_table_name = 'agent_sessions' then
        insert into storage_deletions (prefix)
        values ('sessions/' || old.workspace_id || '/' || old.agent_id || '/' || old.id || '/')
        on conflict do nothing;
    elsif tg_table_name = 'agents' then
        insert into storage_deletions (prefix)
        values ('sessions/' || old.workspace_id || '/' || old.id || '/'),
               ('agents/' || old.workspace_id || '/' || old.id || '/')
        on conflict do nothing;
    elsif tg_table_name = 'workspaces' then
        insert into storage_deletions (prefix)
        values ('sessions/' || old.id || '/'),
               ('agents/' || old.id || '/'),
               ('workspaces/' || old.id || '/')
        on conflict do nothing;
    end if;
    return null;
end
$$;

create trigger agent_sessions_storage_deletion
    after delete on agent_sessions
    for each row execute function storage_deletion_requested();
create trigger agents_storage_deletion
    after delete on agents
    for each row execute function storage_deletion_requested();
create trigger workspaces_storage_deletion
    after delete on workspaces
    for each row execute function storage_deletion_requested();

-- What the idle sweep reads: sessions by how long ago they were last active.
create index agent_sessions_last_active_idx on agent_sessions (last_active_at);
