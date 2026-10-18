-- One table for every kind of trigger, where there was one per kind. See
-- docs/triggers.md, "One model for every trigger".
--
-- What every kind has is a column; what one kind has alone is its `config`:
-- a schedule's expression, zone and count of skipped firings; a webhook's
-- path, scheme and secret. `next_at` is a column although only schedules set
-- it, because the firing loop orders by it and locks the row it takes, and
-- any later source that fires on a clock -- a mailbox polled -- sets it too.
create table triggers (
    id           uuid        primary key,
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    agent_id     uuid        not null references agents (id) on delete cascade,
    kind         text        not null check (kind in ('schedule', 'webhook')),
    name         text        not null,
    -- The message a firing becomes; `{{body}}` where what arrived goes.
    prompt       text        not null,
    enabled      boolean     not null default true,
    account      text,
    owner_id     uuid        references users (id) on delete set null,
    config       jsonb       not null default '{}',
    -- When it next fires, for a kind that fires on a clock.
    next_at      timestamptz,
    -- The hourly ceiling, where the kind has one; a fixed window.
    max_per_hour int         check (max_per_hour > 0),
    window_start timestamptz not null default now(),
    window_count int         not null default 0,
    -- Firings the ceiling turned away, never reset.
    refused      int         not null default 0,
    last_at      timestamptz,
    last_status  text        check (last_status in ('ok', 'failed', 'skipped', 'refused')),
    last_error   text,
    created_by   uuid        references users (id) on delete set null,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

create index triggers_agent_idx on triggers (workspace_id, agent_id);
create index triggers_due_idx on triggers (next_at) where enabled and next_at is not null;
create unique index triggers_webhook_path_idx on triggers ((config->>'path')) where kind = 'webhook';

-- What each firing was, by what its sender calls it, so a firing delivered
-- twice runs once. Recorded in the transaction that queues its turn.
create table trigger_firings (
    trigger_id  uuid        not null references triggers (id) on delete cascade,
    external_id text        not null,
    created_at  timestamptz not null default now(),
    primary key (trigger_id, external_id)
);

insert into triggers (id, workspace_id, agent_id, kind, name, prompt, enabled, account,
                      owner_id, config, next_at, last_at, last_status, last_error,
                      created_by, created_at, updated_at)
select id, workspace_id, agent_id, 'schedule', name, prompt, enabled, account, owner_id,
       jsonb_build_object('expression', expression, 'timezone', timezone, 'skipped', skipped),
       next_run_at, last_run_at, last_status, last_error, created_by, created_at, updated_at
from schedules;

insert into triggers (id, workspace_id, agent_id, kind, name, prompt, enabled, account,
                      owner_id, config, max_per_hour, window_start, window_count, refused,
                      last_at, last_status, last_error, created_by, created_at, updated_at)
select id, workspace_id, agent_id, 'webhook', name, prompt, enabled, account, owner_id,
       jsonb_build_object('path', path, 'scheme', scheme, 'secret', secret),
       max_per_hour, window_start, window_count, refused,
       last_at, last_status, last_error, created_by, created_at, updated_at
from webhook_triggers;

-- A session names the trigger that started it in one column, whatever kind.
alter table agent_sessions
    add column trigger_id uuid references triggers (id) on delete set null;
update agent_sessions set trigger_id = coalesce(schedule_id, webhook_trigger_id)
where schedule_id is not null or webhook_trigger_id is not null;
alter table agent_sessions drop column schedule_id, drop column webhook_trigger_id;

alter table webhook_deliveries drop constraint webhook_deliveries_trigger_id_fkey;
alter table webhook_deliveries add constraint webhook_deliveries_trigger_id_fkey
    foreign key (trigger_id) references triggers (id) on delete cascade;

drop table schedules;
drop table webhook_triggers;
