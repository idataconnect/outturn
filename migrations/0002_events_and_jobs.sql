-- Event log ------------------------------------------------------------------

-- Append-only feed the UI long-polls. `seq` is a bigserial so clients can hold
-- a simple monotonic cursor and ask for anything newer; it also means a missed
-- NOTIFY costs one poll cycle rather than a lost update.
create table events (
    seq         bigserial primary key,
    tenant_id   uuid        not null references tenants (id) on delete cascade,
    session_id  uuid,
    kind        text        not null,
    payload     jsonb       not null default '{}'::jsonb,
    created_at  timestamptz not null default now()
);

create index events_tenant_seq_idx on events (tenant_id, seq);
create index events_session_seq_idx on events (session_id, seq) where session_id is not null;

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
