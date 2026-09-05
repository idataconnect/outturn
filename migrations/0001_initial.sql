-- Extensions -----------------------------------------------------------------

create extension if not exists vector;

-- Tenants --------------------------------------------------------------------

create table tenants (
    id          uuid primary key,
    name        text        not null,
    slug        text        not null unique,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

-- Accounts -------------------------------------------------------------------

-- The account itself carries no credentials and no email: those are ways of
-- proving identity, not the identity. Roles and ownership attach here, so an
-- account survives adding, changing or removing any way of signing in.
create table users (
    id            uuid primary key,
    display_name  text        not null,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);

-- One row per way of signing in: a password login, a magic-link address, an
-- OAuth provider. Several may point at one account, which is what makes
-- account linking and multiple emails work without special cases.
create table user_identities (
    id               uuid        primary key,
    user_id          uuid        not null references users (id) on delete cascade,

    -- 'password' and 'magic_link' use the email address as the subject;
    -- OAuth providers use their own stable subject id.
    provider         text        not null
                     check (provider in ('password', 'magic_link', 'oauth_google', 'oauth_github')),
    provider_subject text        not null,

    -- Credentials live on the identity, not the account: two emails on one
    -- account are separate logins, each with its own secret (or none, for
    -- providers that hold the secret themselves).
    password_hash    text,

    -- Verification is a property of this identity, not of the account.
    verified_at      timestamptz,
    created_at       timestamptz not null default now(),
    updated_at       timestamptz not null default now(),

    -- Globally unique, not per-user: two accounts must never be able to claim
    -- the same address, or linking becomes ambiguous.
    unique (provider, provider_subject)
);

create index user_identities_user_idx on user_identities (user_id);

-- A verified identity is the one that may claim an address; unverified rows
-- must not be able to squat one. Enforced in application code, since a partial
-- unique index cannot express "at most one verified per subject across
-- providers" cleanly.
create index user_identities_subject_idx on user_identities (provider_subject);

-- Sessions -------------------------------------------------------------------

-- Refresh tokens are an allowlist: a row must exist for the token to be
-- accepted, so revoking a session is a delete. This is what makes rotation
-- reuse-detectable and lets a user enumerate and end their own sessions --
-- neither is possible with a denylist of revoked tokens.
--
-- Only a hash is stored: a leaked database must not yield usable tokens.
create table refresh_tokens (
    id            uuid        primary key,
    user_id       uuid        not null references users (id) on delete cascade,
    tenant_id     uuid        not null references tenants (id) on delete cascade,

    token_hash    text        not null unique,

    -- Rotation: each refresh issues a new token and retires the old one. All
    -- descendants of one login share a family, so a replayed token can take
    -- the whole family down with it.
    family_id     uuid        not null,

    -- Set when this token is rotated away. A presented token that is already
    -- rotated is a replay: the family is compromised and must be revoked.
    rotated_at    timestamptz,
    revoked_at    timestamptz,

    -- Context for a "your sessions" view and for spotting theft.
    user_agent    text,
    created_at    timestamptz not null default now(),
    expires_at    timestamptz not null
);

create index refresh_tokens_family_idx on refresh_tokens (family_id);
create index refresh_tokens_user_idx on refresh_tokens (user_id);
-- Supports sweeping tokens that are past use.
create index refresh_tokens_expiry_idx on refresh_tokens (expires_at);

-- Role grants ----------------------------------------------------------------

-- Roles are stored as text and resolved to authorities in application code
-- (see src/auth/rbac.rs). The check constraints keep typos out of the tables
-- without pinning the authority mapping to the schema.
--
-- Grants are split by scope rather than distinguished by a nullable column:
-- system roles are not tenant-scoped, tenant roles always are. Both attach to
-- the account, so which identity was used to sign in never changes access.

create table user_system_roles (
    user_id     uuid        not null references users (id) on delete cascade,
    role        text        not null check (role in ('system_admin')),
    created_at  timestamptz not null default now(),
    primary key (user_id, role)
);

create table user_tenant_roles (
    user_id     uuid        not null references users (id) on delete cascade,
    tenant_id   uuid        not null references tenants (id) on delete cascade,
    role        text        not null check (role in ('admin', 'operator', 'viewer')),
    created_at  timestamptz not null default now(),
    primary key (user_id, tenant_id, role)
);

create index user_tenant_roles_tenant_idx on user_tenant_roles (tenant_id);

-- Agents ---------------------------------------------------------------------

-- Agents belong to exactly one tenant. Every query is scoped by the tenant on
-- the caller's token rather than by anything in the request, so reaching
-- another tenant's agents requires holding a token minted for that tenant --
-- which in turn requires a role grant there.
create table agents (
    id             uuid        primary key,
    tenant_id      uuid        not null references tenants (id) on delete cascade,

    name           text        not null,
    slug           text        not null,
    description    text        not null default '',

    -- Prepended to every conversation this agent runs.
    system_prompt  text        not null default '',

    -- Security policy lives here as it develops; HITL approval rules are the
    -- expected first occupant. Kept as jsonb so policy can evolve without a
    -- migration per field.
    policy         jsonb       not null default '{}'::jsonb,

    enabled        boolean     not null default true,
    created_at     timestamptz not null default now(),
    updated_at     timestamptz not null default now(),

    -- Unique per tenant, not globally: two orgs may both have a "support"
    -- agent without knowing about each other.
    unique (tenant_id, slug)
);

create index agents_tenant_idx on agents (tenant_id);

-- Agent sessions -------------------------------------------------------------

-- A session is one conversation with an agent: starting a new session is how a
-- user gets a fresh context. Tenant-scoped like everything else.
create table agent_sessions (
    id          uuid        primary key,
    tenant_id   uuid        not null references tenants (id) on delete cascade,
    agent_id    uuid        not null references agents (id) on delete cascade,
    -- Who started it, for attribution. Kept when the account goes away so the
    -- transcript is not silently rewritten.
    user_id     uuid        references users (id) on delete set null,
    title       text        not null default '',
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

create index agent_sessions_tenant_idx on agent_sessions (tenant_id, created_at desc);
create index agent_sessions_agent_idx on agent_sessions (agent_id);

-- The conversation itself. Ordered by `id`, which is a UUIDv7: the ordering
-- is carried by the key itself, so there is no separate sequence to assign and
-- concurrent inserts cannot interleave ambiguously.
create table agent_messages (
    id           uuid        primary key,
    session_id   uuid        not null references agent_sessions (id) on delete cascade,
    role         text        not null check (role in ('system', 'user', 'assistant', 'tool')),
    content      text        not null,

    -- Who sent it. On the session too, but recorded per message because a
    -- session can be posted into by more than one person and "who asked
    -- this" is the question usage attribution actually needs answering.
    -- Kept when the account goes away, like the session's own reference.
    user_id      uuid        references users (id) on delete set null,

    -- Which endpoint produced this reply: "openai:https://api.openai.com".
    -- Protocol and base URL rather than a vendor name, because the same
    -- protocol serves several, and spend attaches to the endpoint that
    -- billed for it. Also what says whether a stored provider artifact --
    -- a thought signature, a thinking block -- may be replayed.
    provider     text,

    -- How this message should reach a turn that is already running.
    --
    -- "steer" is injected at the next round boundary, so the agent redirects
    -- mid-work. "follow_up" waits until the agent would otherwise stop and
    -- extends the turn instead of ending it. The distinction only matters
    -- while something is in flight; a message arriving into a quiet session
    -- simply starts a turn either way.
    delivery     text        not null default 'steer'
                 check (delivery in ('steer', 'follow_up')),

    -- The reply that took this message mid-turn, if one did. A steered
    -- message is answered inside the turn it interrupted, so the turn queued
    -- for it must know not to answer it again.
    absorbed_by  uuid        references agent_messages (id) on delete set null,

    -- The message this one answers, for a reply. A turn is retried when a
    -- worker dies mid-generation, and without this the retry would create a
    -- second empty reply and orphan the first -- which then sits in the
    -- transcript being replayed to the model forever. The unique index makes
    -- that impossible in the database rather than by remembering to check,
    -- and it scopes ownership to the prompt, so two turns running at once in
    -- one session cannot claim each other's reply.
    replies_to   uuid        references agent_messages (id) on delete cascade,

    -- What the agent did on the way to this reply: tool calls, each with the
    -- model's own reason for making it. Kept with the message rather than as
    -- events, so reopening a session shows the work and not just the answer.
    metadata     jsonb       not null default '{}'::jsonb,

    -- Usage attribution. Null until a provider reports it; the model is
    -- recorded per message because an agent's model may change between turns.
    model        text,

    -- Split because the parts are priced differently and none of them can be
    -- worked out from the text. A cached prompt costs less than a fresh one;
    -- thinking is billed as output but reported apart. Counting tokens here
    -- could never tell them apart, which is why these come from the provider.
    prompt_tokens       int,   -- billed at full rate, cache excluded
    completion_tokens   int,   -- all output, thinking included
    cache_read_tokens   int,
    cache_write_tokens  int,
    reasoning_tokens    int,

    created_at   timestamptz not null default now()
);

create index agent_messages_session_idx on agent_messages (session_id, id);

-- One reply per prompt. This is the constraint that makes a retry idempotent.
create unique index agent_messages_replies_to_idx
    on agent_messages (replies_to) where replies_to is not null;

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
    -- Work that must not run beside itself. At most one job per key is
    -- running at a time, so turns in one conversation are answered in order
    -- rather than in parallel -- two at once would each be generated against
    -- a history that did not contain the other, and the transcript would
    -- claim a causality that never happened. Null means unconstrained.
    serial_key    text,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);

-- Supports the claim query: pending work whose time has come, oldest first.
create index jobs_claimable_idx on jobs (run_after, id)
    where state = 'pending';

-- Supports the serialisation check, which asks whether a key is already
-- running before claiming another job for it.
create index jobs_serial_running_idx on jobs (serial_key)
    where state = 'running' and serial_key is not null;

-- Supports reaping abandoned leases.
create index jobs_lease_idx on jobs (leased_until)
    where state = 'running';

-- What an autoscaler should read: work that could start now, not work that is
-- waiting. A serial key admits one running job at a time, so a session with a
-- hundred queued turns is one unit of work rather than a hundred -- counting
-- rows would ask for pods that cannot claim anything.
--
-- Defined here rather than in the scaler's configuration so there is one
-- statement of what "backlog" means, and so a test can hold it against what
-- `claim` actually takes.
--
-- The window admits work deferred by a few seconds, because a turn a full
-- runtime handed back is precisely the signal to scale on, and excluding it
-- would make the backlog look shortest when the cluster is busiest. It stops
-- short of genuinely scheduled work, which must not hold pods open overnight.
create view job_backlog as
select kind, sum(units)::bigint as claimable
from (
    -- Unconstrained work: every row is its own unit.
    select kind, count(*) as units
      from jobs
     where state = 'pending'
       and run_after <= now() + interval '30 seconds'
       and serial_key is null
     group by kind
    union all
    -- Serialised work: one unit per key, and none for a key already running.
    select j.kind, count(distinct j.serial_key) as units
      from jobs j
     where j.state = 'pending'
       and j.run_after <= now() + interval '30 seconds'
       and j.serial_key is not null
       and not exists (
           select 1 from jobs r
            where r.state = 'running'
              and r.serial_key = j.serial_key
       )
     group by j.kind
) parts
group by kind;

-- This table is high-churn: rows are updated on claim and again on completion,
-- so dead tuples accumulate faster than the default autovacuum thresholds
-- expect.
alter table jobs set (
    autovacuum_vacuum_scale_factor = 0.02,
    autovacuum_analyze_scale_factor = 0.02
);
