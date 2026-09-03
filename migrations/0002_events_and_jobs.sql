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
