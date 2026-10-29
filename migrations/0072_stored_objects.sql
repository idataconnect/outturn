-- What each workspace holds in object storage, for its storage quota. See
-- docs/storage-quota.md.
--
-- A row per object rather than a counter, because writes are not purely
-- additive: overwriting ten megabytes with one lowers the total by nine, and
-- only a per-key size knows that without asking the store before every write.
-- The total beside each workspace is kept by the trigger, so concurrent turns
-- never read-modify-write it in application code.
create table stored_objects (
    key          text        primary key,
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    bytes        bigint      not null check (bytes >= 0),
    written_at   timestamptz not null default now()
);

create index stored_objects_workspace_idx on stored_objects (workspace_id);

alter table workspaces
    add column stored_bytes bigint not null default 0,
    -- When the bucket was last listed for this workspace. Null until the first
    -- listing, and until then no quota is enforced: a count that has never
    -- been checked against the bucket would refuse writes on a wrong number.
    add column stored_counted_at timestamptz;

create function stored_objects_total() returns trigger
language plpgsql as $$
begin
    if tg_op = 'INSERT' then
        update workspaces set stored_bytes = stored_bytes + new.bytes
         where id = new.workspace_id;
    elsif tg_op = 'UPDATE' then
        update workspaces set stored_bytes = stored_bytes + new.bytes - old.bytes
         where id = new.workspace_id;
    else
        update workspaces set stored_bytes = greatest(stored_bytes - old.bytes, 0)
         where id = old.workspace_id;
    end if;
    return null;
end
$$;

create trigger stored_objects_total
    after insert or update or delete on stored_objects
    for each row execute function stored_objects_total();
