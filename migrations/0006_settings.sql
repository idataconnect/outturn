-- Overrides for the settings catalogue in code. See docs/settings.md.
--
-- The catalogue (src/api/settings/mod.rs) says what settings exist, their
-- types, their defaults and who may override them. This table holds only the
-- levels that have chosen to differ: a row is the "override" toggle being on,
-- and deleting it is the toggle going off, so that level falls back to
-- whatever is above it. Nothing is ever copied down.
--
-- Three levels share one table: the operator's system defaults (tenant_id is
-- the platform tenant, agent_id nil), a tenant's (its own tenant_id, agent_id
-- nil), and an agent's (tenant_id and agent_id). Nil rather than null for
-- "no agent" so the primary key can carry it.
create table setting_overrides (
    tenant_id  uuid        not null references tenants (id) on delete cascade,
    agent_id   uuid        not null default '00000000-0000-0000-0000-000000000000',
    key        text        not null,
    value      jsonb       not null,
    updated_at timestamptz not null default now(),
    primary key (tenant_id, agent_id, key)
);

-- Agent rows go when the agent does. A plain foreign key cannot express
-- "nil or a real agent", so a trigger does.
create or replace function drop_agent_setting_overrides() returns trigger as $$
begin
    delete from setting_overrides where tenant_id = old.tenant_id and agent_id = old.id;
    return old;
end;
$$ language plpgsql;

create trigger agent_setting_overrides_go_with_the_agent
    after delete on agents
    for each row execute function drop_agent_setting_overrides();
