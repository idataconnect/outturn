-- Event log ------------------------------------------------------------------

-- Append-only feed the UI long-polls. `id` is a UUIDv7, so it is both the key
-- and the cursor: clients ask for anything greater than the last id they saw,
-- and a missed NOTIFY costs one poll cycle rather than a lost update.
--
-- Cursors are safe here because writes to a single session's feed are
-- serialised by the job lease -- one worker owns a session's turn at a time.
-- Without that, a row could commit below a cursor another poller had already
-- passed, and be missed.
create table events (
    id          uuid        primary key,
    tenant_id   uuid        not null references tenants (id) on delete cascade,
    session_id  uuid,
    kind        text        not null,
    payload     jsonb       not null default '{}'::jsonb,
    created_at  timestamptz not null default now()
);

create index events_tenant_id_idx on events (tenant_id, id);
create index events_session_id_idx on events (session_id, id) where session_id is not null;

-- Provider health -------------------------------------------------------------

-- A circuit breaker shared by every replica.
--
-- Held in the database rather than in each process because the failure it
-- guards against is collective: a provider goes down, and every gateway pod
-- independently discovers it and independently keeps probing. One pod backing
-- off achieves nothing while its neighbours hammer the same dead endpoint.
--
-- Keyed on protocol *and* base URL. Provider names a wire format now, not a
-- vendor -- api.openai.com and a local ollama both speak the OpenAI protocol,
-- and one being down says nothing about the other.
create table provider_health (
    endpoint     text        primary key,
    state        text        not null default 'closed'
                 check (state in ('closed', 'open', 'half_open')),
    failures     int         not null default 0,
    -- When a probe may next be attempted. Moved forward by whichever replica
    -- claims the probe, so the others keep rejecting rather than joining in.
    probe_after  timestamptz,
    last_error   text,
    opened_at    timestamptz,
    updated_at   timestamptz not null default now()
);

-- Traffic routing ------------------------------------------------------------

-- Where a class of traffic is sent, in the order to try.
--
-- Traffic type names what the work is for -- "assistant" for a user waiting on
-- a reply, and later cheaper classes for titles, summaries and compaction. The
-- caller pins the type and the gateway resolves the route, because only the
-- gateway knows which endpoints are currently healthy.
--
-- Each row is one attempt: a protocol, where to reach it, and which model to
-- ask for. They are tried by priority, skipping any whose circuit is open, so
-- failover walks the list rather than needing a second mechanism.
--
-- No credentials here. The gateway is the tier that holds them precisely so
-- they live in one place; `credential_ref` names an environment variable and
-- the value is resolved at construction. A base URL and a model name in the
-- database are configuration; an API key would be a leak waiting to happen.
create table traffic_routes (
    id             uuid        primary key,
    -- Null means the system default, used by any tenant without its own.
    tenant_id      uuid        references tenants (id) on delete cascade,
    traffic_type   text        not null,
    priority       int         not null,
    provider       text        not null check (provider in ('openai', 'anthropic')),
    base_url       text        not null,
    model          text        not null,
    credential_ref text,
    enabled        boolean     not null default true,
    created_at     timestamptz not null default now()
);

-- A surrogate key rather than the natural one, because a primary key cannot
-- contain nulls and the system defaults are exactly the rows whose tenant is
-- null. Uniqueness is enforced by two partial indexes instead, one for each
-- case, since nulls do not compare equal in a unique index either.
create unique index traffic_routes_tenant_idx
    on traffic_routes (tenant_id, traffic_type, priority) where tenant_id is not null;
create unique index traffic_routes_system_idx
    on traffic_routes (traffic_type, priority) where tenant_id is null;

-- Job queue ------------------------------------------------------------------

-- Worked with SELECT ... FOR UPDATE SKIP LOCKED. Enqueue happens in the same
-- transaction as the state change that caused it, so there is no dual-write
-- to reconcile.
create table jobs (
    id            uuid primary key,
    tenant_id     uuid        not null references tenants (id) on delete cascade,
    kind          text        not null,
    payload       jsonb       not null default '{}'::jsonb,
    state         text        not null default 'pending'
                  check (state in ('pending', 'running', 'succeeded', 'failed')),
    attempts      int         not null default 0,
    max_attempts  int         not null default 3,
    last_error    text,
    -- Delayed and retried work is scheduled by moving this forward.
    run_after     timestamptz not null default now(),
    -- Set when claimed; a claim older than the lease is considered abandoned.
    leased_until  timestamptz,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);

-- Supports the claim query: pending work whose time has come, oldest first.
create index jobs_claimable_idx on jobs (run_after, id)
    where state = 'pending';

-- Supports reaping abandoned leases.
create index jobs_lease_idx on jobs (leased_until)
    where state = 'running';

-- This table is high-churn: rows are updated on claim and again on completion,
-- so dead tuples accumulate faster than the default autovacuum thresholds
-- expect.
alter table jobs set (
    autovacuum_vacuum_scale_factor = 0.02,
    autovacuum_analyze_scale_factor = 0.02
);
