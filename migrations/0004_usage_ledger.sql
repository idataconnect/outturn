-- One row per model call, with every dimension a bill might be cut along.
--
-- Usage used to be summed across a turn and stored on the reply: one set of
-- counts per message, naming the endpoint that served the last round. That
-- cannot carry a bill. A turn that fell back mid-way has two providers and one
-- row; nothing says whose credential paid; and there is no end-customer
-- dimension, so a tenant cannot split its own bill by the customers it serves.
--
-- Append-only. Tokens as the provider reported them, never prices: rate cards
-- change and disputes happen, and a ledger that stored a computed cost would
-- have to be corrected where one that stores tokens is re-priced by whoever
-- is billing. The export is the product -- an operator bills tenants from it,
-- a tenant bills its customers from it -- and nobody needs a billing system
-- inside outturn.

-- Work the platform does on its own initiative -- titles, summaries, whatever
-- comes -- bills to a tenant that is the operator. A reserved row rather than
-- a null keeps the ledger's partitioning uniform and its foreign keys real.
insert into tenants (id, name, slug)
values ('00000000-0000-0000-0000-000000000001', 'Platform', 'platform')
on conflict (id) do nothing;

create or replace function protect_platform_tenant() returns trigger as $$
begin
    if old.id = '00000000-0000-0000-0000-000000000001' then
        raise exception 'the platform tenant cannot be deleted';
    end if;
    return old;
end;
$$ language plpgsql;

create trigger platform_tenant_stays
    before delete on tenants
    for each row execute function protect_platform_tenant();

-- Which of the tenant's own customers a conversation is for. The platform
-- does not know what an account is, only that a session may carry one and
-- the ledger copies it, so a tenant can join its bill to its own records.
alter table agent_sessions add column account text;

create table usage_ledger (
    tenant_id          uuid        not null references tenants (id) on delete cascade,
    id                 uuid        not null,
    occurred_at        timestamptz not null default now(),

    -- Who.
    agent_id           uuid,
    session_id         uuid,
    user_id            uuid,
    account            text,

    -- What produced it.
    reply_id           uuid,
    job_id             uuid,
    -- Which model call within the turn, from zero.
    round              int         not null,
    traffic_type       text        not null,
    -- "openai:https://api.openai.com": protocol and base URL, as the gateway
    -- reports it.
    endpoint           text        not null,
    -- The model that actually answered, which routing may have chosen.
    model              text        not null,
    -- Whose key paid: 'operator' or 'tenant'.
    credential_owner   text        not null,
    -- 'none', 'same_model' or 'cross_model'. See docs/routing.md.
    fallback           text        not null default 'none',

    -- How much, in the units it is billed in. See the usage record in
    -- wit/agent.wit for why these are split.
    prompt_tokens      int         not null default 0,
    completion_tokens  int         not null default 0,
    cache_read_tokens  int         not null default 0,
    cache_write_tokens int         not null default 0,
    reasoning_tokens   int         not null default 0,

    primary key (tenant_id, id)
) partition by hash (tenant_id);

do $$
begin
    for i in 0..15 loop
        execute format(
            'create table usage_ledger_p%s partition of usage_ledger '
            'for values with (modulus 16, remainder %s)', i, i);
    end loop;
end $$;

-- The export: a tenant's rows in time order, paged by id. The id is a UUIDv7
-- so it orders by time and doubles as the cursor.
create index usage_ledger_export_idx on usage_ledger (tenant_id, id);
create index usage_ledger_session_idx on usage_ledger (tenant_id, session_id);

-- Reading the ledger is its own authority. The default admin role gets it,
-- since the admin is who cuts the bill; other roles can be given it.
insert into role_authorities (tenant_id, role_id, authority)
select tenant_id, id, 'usage:read' from roles where name = 'admin'
on conflict do nothing;
