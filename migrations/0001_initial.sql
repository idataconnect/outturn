-- Extensions -----------------------------------------------------------------

create extension if not exists vector;

-- Workspaces -------------------------------------------------------------------

create table workspaces (
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
    workspace_id  uuid        not null references workspaces (id) on delete cascade,

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
-- system roles are not workspace-scoped, workspace roles always are. Both
-- attach to the account, so which identity was used to sign in never changes
-- access.

create table user_system_roles (
    user_id     uuid        not null references users (id) on delete cascade,
    role        text        not null check (role in ('system_admin')),
    created_at  timestamptz not null default now(),
    primary key (user_id, role)
);

-- Roles become the workspace's to define.
--
-- Authorities are the fixed vocabulary in code; a role bundles them, and
-- which bundles exist and what they are called is a workspace's business.
-- Every workspace starts with copies of the defaults the code used to
-- hard-code, and may edit them from there.
--
-- Workspace-scoped on every row, with composite keys, so a role's authority
-- row cannot point at another workspace's role and row-level security can be
-- switched on later with a policy rather than a rewrite. The platform's own
-- roles (system_admin, runtime, turn) are not rows: they stay in code, so
-- this table holds one kind of thing with one owner.

create table roles (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    id           uuid        not null,
    name         text        not null,
    description  text        not null default '',
    created_at   timestamptz not null default now(),
    primary key (workspace_id, id),
    unique (workspace_id, name)
);

create table role_authorities (
    workspace_id uuid not null,
    role_id      uuid not null,
    -- Validated against the Authority enum in code on write, not here: the
    -- vocabulary changes with the code, and a check constraint would need a
    -- migration every time it did.
    authority    text not null,
    primary key (workspace_id, role_id, authority),
    foreign key (workspace_id, role_id) references roles (workspace_id, id) on delete cascade
);

create table user_workspace_roles (
    user_id      uuid        not null references users (id) on delete cascade,
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    role_id      uuid        not null,
    created_at   timestamptz not null default now(),
    primary key (user_id, workspace_id, role_id),
    foreign key (workspace_id, role_id) references roles (workspace_id, id) on delete cascade
);

create index user_workspace_roles_workspace_idx on user_workspace_roles (workspace_id);

-- Agents ---------------------------------------------------------------------

-- Agents belong to exactly one workspace. Every query is scoped by the
-- workspace on the caller's token rather than by anything in the request, so
-- reaching another workspace's agents requires holding a token minted for
-- that workspace -- which in turn requires a role grant there.
create table agents (
    id             uuid        primary key,
    workspace_id   uuid        not null references workspaces (id) on delete cascade,

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

    -- Unique per workspace, not globally: two orgs may both have a "support"
    -- agent without knowing about each other.
    unique (workspace_id, slug)
);

create index agents_workspace_idx on agents (workspace_id);

-- Agent sessions -------------------------------------------------------------

-- A session is one conversation with an agent: starting a new session is how a
-- user gets a fresh context. Workspace-scoped like everything else.
create table agent_sessions (
    id            uuid        primary key,
    workspace_id  uuid        not null references workspaces (id) on delete cascade,
    agent_id      uuid        not null references agents (id) on delete cascade,
    -- Who started it, for attribution. Kept when the account goes away so the
    -- transcript is not silently rewritten.
    user_id       uuid        references users (id) on delete set null,
    title         text        not null default '',
    -- Which of the workspace's own customers this conversation is for. The
    -- platform does not know what an account is, only that a session may
    -- carry one and the ledger copies it, so a workspace can join its bill to
    -- its own records.
    account       text,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);

create index agent_sessions_workspace_idx on agent_sessions (workspace_id, created_at desc);
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

    -- What the agent did on the way to this reply, and in what order.
    --
    -- `tool_calls` holds each call with the model's own reason for making it.
    -- `parts` holds the sequence: text where it was said, and a call named by
    -- id where it was made. A reply is a sequence, not prose with calls
    -- attached -- an agent asked to say what it is about to do says it before
    -- it does it -- and the arrangement cannot be recovered once the pieces
    -- are sorted into "all the text" and "all the calls".
    --
    -- Kept with the message rather than as events, so reopening a session
    -- shows the work and not just the answer. Text lives in `parts`; a call
    -- lives in `tool_calls` and is only referenced, so nothing is written
    -- twice. `content` is the text of the parts joined, for everything that
    -- wants the prose and not the shape.
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
    id            uuid        primary key,
    workspace_id  uuid        not null references workspaces (id) on delete cascade,
    session_id    uuid,
    kind          text        not null,
    payload       jsonb       not null default '{}'::jsonb,
    created_at    timestamptz not null default now()
);

create index events_workspace_id_idx on events (workspace_id, id);
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
    -- Null means the system default, used by any workspace without its own.
    workspace_id   uuid        references workspaces (id) on delete cascade,
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
-- contain nulls and the system defaults are exactly the rows whose workspace
-- is null. Uniqueness is enforced by two partial indexes instead, one for
-- each case, since nulls do not compare equal in a unique index either.
create unique index traffic_routes_workspace_idx
    on traffic_routes (workspace_id, traffic_type, priority) where workspace_id is not null;
create unique index traffic_routes_system_idx
    on traffic_routes (traffic_type, priority) where workspace_id is null;

-- Job queue ------------------------------------------------------------------

-- Worked with SELECT ... FOR UPDATE SKIP LOCKED. Enqueue happens in the same
-- transaction as the state change that caused it, so there is no dual-write
-- to reconcile.
-- Hosts a workspace's agents may reach.
--
-- Empty means an agent reaches nothing, which is the default and the point: a
-- workspace that has not thought about egress has not consented to it. Adding
-- a row is the whole of the ceremony, and it is about a hostname rather than a
-- URL because that is the part a workspace knows without guessing.
create table egress_rules (
    id             uuid        primary key,
    workspace_id   uuid        not null references workspaces (id) on delete cascade,
    -- `api.stripe.com`, or `*.example.com` for its subdomains but not its apex.
    host           text        not null,
    -- The header a credential travels in, attached by the host on the way out.
    -- Null for an API that needs none.
    header         text,
    -- The name of the environment variable holding that header's value, never
    -- the value. Secrets stay where the platform already keeps them rather
    -- than in a row that a backup, a log line or a support query carries off,
    -- and nothing that reads this table can leak one by reading it.
    credential_env text,
    enabled        boolean     not null default true,
    created_at     timestamptz not null default now(),
    -- One rule per host per workspace: two rules for one host would differ
    -- only in which credential they attached, and which won would depend on
    -- insertion order.
    unique (workspace_id, host)
);

create table jobs (
    id            uuid primary key,
    workspace_id  uuid        not null references workspaces (id) on delete cascade,
    kind          text        not null,
    payload       jsonb       not null default '{}'::jsonb,
    state         text        not null default 'pending'
                  check (state in ('pending', 'running', 'succeeded', 'failed')),
    attempts      int         not null default 0,
    max_attempts  int         not null default 3,
    -- What is waiting on this, and therefore what it costs to be late.
    --
    -- A number rather than a class, so a third band needs new values and not
    -- new SQL. Lower is sooner. Two are used: work somebody is waiting for,
    -- and work that only has to happen eventually -- because that difference,
    -- not the size of the job, is what decides whether a queue is a problem.
    --
    -- Claim order reads this before `run_after`, so a backlog of scheduled
    -- work cannot put itself in front of a person. It cannot preempt a turn
    -- already running: the guarantee is that the next slot to free anywhere in
    -- the fleet goes to the higher priority, which is bounded by the shortest
    -- turn in flight rather than by how long a pod takes to start.
    priority      int         not null default 100,
    -- Which claim currently holds this job. Reissued on every claim, so a
    -- heartbeat from a claim that was reaped renews nothing: without it, the
    -- previous holder's renewal extends whichever claim is current and is told
    -- it still owns the job, and two workers stream the same turn into the
    -- same reply.
    lease_token   uuid,
    -- Times this job was handed back because nowhere had room to run it.
    -- Counted apart from attempts, which it gives back: a cluster that is
    -- merely busy must not exhaust a job's retries without running it, but a
    -- cluster that is permanently full must not spin on it forever either.
    releases      int         not null default 0,
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

-- Supports the claim query: pending work whose time has come, most urgent
-- first and oldest first within that.
create index jobs_claimable_idx on jobs (priority, run_after, id)
    where state = 'pending';

-- Supports the serialisation check, which asks whether a key is already
-- running before claiming another job for it.
create index jobs_serial_running_idx on jobs (serial_key)
    where state = 'running' and serial_key is not null;

-- Supports reaping abandoned leases.
create index jobs_lease_idx on jobs (leased_until)
    where state = 'running';

-- The transcript reports, per user message, the state of the job answering
-- it -- so a reader can be told "queued", "failed" or nothing rather than
-- guessing from an empty reply. That is a lookup by the message id inside the
-- payload, which without this is a scan of every turn ever queued.
create index jobs_chat_turn_message_idx
    on jobs (((payload->>'message_id')::uuid))
    where kind = 'chat.turn';

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
select kind, priority, sum(units)::bigint as claimable
from (
    -- Unconstrained work: every row is its own unit.
    select kind, priority, count(*) as units
      from jobs
     where state = 'pending'
       and run_after <= now() + interval '30 seconds'
       and serial_key is null
     group by kind, priority
    union all
    -- Serialised work: one unit per key, and none for a key already running.
    select j.kind, j.priority, count(distinct j.serial_key) as units
      from jobs j
     where j.state = 'pending'
       and j.run_after <= now() + interval '30 seconds'
       and j.serial_key is not null
       and not exists (
           select 1 from jobs r
            where r.state = 'running'
              and r.serial_key = j.serial_key
       )
     group by j.kind, j.priority
) parts
group by kind, priority;

-- Conversations somebody is currently in.
--
-- Kept apart from the transcript on purpose. The count wanted here is "how
-- many people are mid-conversation", and deriving that from `agent_messages`
-- means a count(distinct) over a time range on the busiest table in the
-- system -- which gets more expensive exactly as the cluster gets busier, and
-- which scans every partition once that table is partitioned by workspace.
--
-- Bounded by concurrency rather than by history: a row exists only while a
-- session is live, so this table is the size of the conversations happening
-- now, not of every conversation ever. At that size a sequential scan beats
-- an index, which is why there is no index on it.
--
-- Nor could there usefully be one. A partial index on "recent" needs a
-- predicate over now(), which is not immutable and so cannot be indexed; the
-- alternatives all need something to periodically rewrite the index or reset
-- a flag. Deleting the row instead is the same bookkeeping with none of that.
create table live_sessions (
    session_id uuid        primary key references agent_sessions (id) on delete cascade,
    -- When this session stops counting unless somebody speaks again. Moved
    -- forward on every message, so silence expires it without anything having
    -- to notice.
    expires_at timestamptz not null
);

-- How many runtime pods the work in front of us wants.
--
-- Queue depth alone is a lagging measure: by the time work is queued somebody
-- is already waiting, and a pod that arrives thirty seconds later does not
-- help the turns that queued. So the figure is a floor, plus a term for each
-- kind of demand, weighted by what being late costs.
--
-- Sessions somebody is actually in predict arrivals that have not happened
-- yet, which is the leading half. A session opened yesterday and abandoned
-- predicts nothing, so only recent ones count. Background depth is the
-- lagging half and is allowed to be -- nobody is waiting on it, so a queue
-- there is a queue rather than a problem.
--
-- Read by the autoscaler with a target of one, so the arithmetic lives here
-- where it can be read, rather than being smuggled into a threshold.
create view desired_runtime_pods as
with settings as (
    select
        -- Enough to serve the ordinary case without waiting for a scale-up.
        2::numeric   as floor_pods,
        -- Conversations one pod can hold at once. Should track
        -- OUTTURN_MAX_CONCURRENT_TURNS; a mismatch only makes the estimate
        -- less good, never wrong.
        2::numeric   as conversations_per_pod,
        -- Queued background jobs one pod works through between polls. Larger
        -- than the conversation figure because nobody is waiting.
        8::numeric   as jobs_per_pod
),
live as (
    -- Filtered rather than trusted: a sweep that falls behind makes this
    -- scan slightly larger, never the answer wrong.
    select count(*) as n from live_sessions where expires_at > now()
),
waiting as (
    select
        coalesce(sum(claimable) filter (where priority <= 10), 0) as realtime,
        coalesce(sum(claimable) filter (where priority > 10), 0)  as background
    from job_backlog
)
select
    (settings.floor_pods
      + ceil(live.n / settings.conversations_per_pod)
      + ceil(waiting.realtime / settings.conversations_per_pod)
      + ceil(waiting.background / settings.jobs_per_pod))::int as pods,
    live.n as live_sessions,
    waiting.realtime,
    waiting.background
from settings, live, waiting;

-- This table is high-churn: rows are updated on claim and again on completion,
-- so dead tuples accumulate faster than the default autovacuum thresholds
-- expect.
alter table jobs set (
    autovacuum_vacuum_scale_factor = 0.02,
    autovacuum_analyze_scale_factor = 0.02
);

-- Usage ledger -----------------------------------------------------------------

-- One row per model call, with every dimension a bill might be cut along.
--
-- Append-only. Tokens as the provider reported them, never prices: rate cards
-- change and disputes happen, and a ledger that stored a computed cost would
-- have to be corrected where one that stores tokens is re-priced by whoever
-- is billing. The export is the product -- an operator bills workspaces from
-- it, a workspace bills its customers from it -- and nobody needs a billing
-- system inside outturn.

-- Work the platform does on its own initiative -- titles, summaries, whatever
-- comes -- bills to a workspace that is the operator. A reserved row rather
-- than a null keeps the ledger's partitioning uniform and its foreign keys
-- real.
insert into workspaces (id, name, slug)
values ('00000000-0000-0000-0000-000000000001', 'Platform', 'platform')
on conflict (id) do nothing;

create or replace function protect_platform_workspace() returns trigger as $$
begin
    if old.id = '00000000-0000-0000-0000-000000000001' then
        raise exception 'the platform workspace cannot be deleted';
    end if;
    return old;
end;
$$ language plpgsql;

create trigger platform_workspace_stays
    before delete on workspaces
    for each row execute function protect_platform_workspace();

create table usage_ledger (
    workspace_id       uuid        not null references workspaces (id) on delete cascade,
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
    -- Whose key paid: 'operator' or 'workspace'.
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

    -- The provider's usage object, verbatim, beside the normalised columns.
    --
    -- The five token columns are what every provider agrees on and every rate
    -- card needs. They are not a superset and never will be: cache writes
    -- priced by TTL, service tiers, long-context thresholds, server-side
    -- tools billed per call, audio and image tokens -- each provider adds
    -- dimensions on its own schedule. Chasing them as columns is a migration
    -- per release and a schema still behind.
    --
    -- So the raw object is kept as it came off the wire. The normalised
    -- columns build today's bill; the raw object lets someone re-price
    -- yesterday's calls under a dimension nobody thought to normalise,
    -- without a backfill, because the data was never dropped.
    provider_usage     jsonb,
    -- Normalized on its own because it changes the price of every other
    -- number on the row: OpenAI's flex and priority tiers bill the same
    -- tokens at different rates.
    service_tier       text,

    primary key (workspace_id, id)
) partition by hash (workspace_id);

do $$
begin
    for i in 0..15 loop
        execute format(
            'create table usage_ledger_p%s partition of usage_ledger '
            'for values with (modulus 16, remainder %s)', i, i);
    end loop;
end $$;

-- The export: a workspace's rows in time order, paged by id. The id is a
-- UUIDv7 so it orders by time and doubles as the cursor.
create index usage_ledger_export_idx on usage_ledger (workspace_id, id);
create index usage_ledger_session_idx on usage_ledger (workspace_id, session_id);

-- Settings ---------------------------------------------------------------------

-- Overrides for the settings catalogue in code. See docs/settings.md.
--
-- The catalogue (src/api/settings/mod.rs) says what settings exist, their
-- types, their defaults and who may override them. This table holds only the
-- levels that have chosen to differ: a row is the "override" toggle being on,
-- and deleting it is the toggle going off, so that level falls back to
-- whatever is above it. Nothing is ever copied down.
--
-- Three levels share one table: the operator's system defaults (workspace_id
-- is the platform workspace, agent_id nil), a workspace's (its own
-- workspace_id, agent_id nil), and an agent's (workspace_id and agent_id).
-- Nil rather than null for "no agent" so the primary key can carry it.
create table setting_overrides (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    agent_id     uuid        not null default '00000000-0000-0000-0000-000000000000',
    key          text        not null,
    value        jsonb       not null,
    updated_at   timestamptz not null default now(),
    primary key (workspace_id, agent_id, key)
);

-- Agent rows go when the agent does. A plain foreign key cannot express
-- "nil or a real agent", so a trigger does.
create or replace function drop_agent_setting_overrides() returns trigger as $$
begin
    delete from setting_overrides where workspace_id = old.workspace_id and agent_id = old.id;
    return old;
end;
$$ language plpgsql;

create trigger agent_setting_overrides_go_with_the_agent
    after delete on agents
    for each row execute function drop_agent_setting_overrides();

-- Skills -----------------------------------------------------------------------

-- Prose an agent is given alongside its system prompt: how to drive a tool,
-- how this workspace wants a job done.
--
-- Kept in the database rather than in object storage for two reasons. An agent
-- holds write access to its workspace's files, so a skill living there would be
-- one the agent could rewrite mid-turn; and an eval is worth nothing unless the
-- exact text that ran can be named afterwards, which a mutable path cannot do.
--
-- Ownership is the platform workspace for a skill the operator ships to
-- everyone, and the workspace's own id for one it wrote itself -- the same
-- split setting_overrides uses to tell an operator default from a workspace's.
create table skills (
    id           uuid primary key,
    workspace_id uuid not null references workspaces (id) on delete cascade,
    slug         text not null,
    name         text not null,
    description  text not null default '',

    -- 'standalone' is prose in its own right, whoever owns it. 'override' is a
    -- workspace's instructions layered over somebody else's skill when the
    -- prompt is composed; it is never merged into the base and means nothing
    -- without it.
    kind         text not null default 'standalone'
                 check (kind in ('standalone', 'override')),

    -- What an override layers onto. Restricted rather than cascading: retiring
    -- is how a skill is withdrawn, and a delete that reached across into a
    -- workspace's own writing would be the operator destroying a customer's
    -- work to tidy up their own.
    base_skill_id uuid references skills (id) on delete restrict,

    -- Where a fork was taken, and from which version. Provenance only: nothing
    -- is merged back, because two people editing the same prose conflict in
    -- ways no algorithm should be trusted to settle silently. Keeping the exact
    -- ancestor is what lets a fork be diffed against what the base has done
    -- since -- and it is the common ancestor a three-way merge would need if
    -- one is ever offered.
    forked_from_skill_id   uuid references skills (id) on delete set null,
    forked_from_version_id uuid,

    -- Withdrawn rather than deleted. Bindings that exist keep working and no
    -- new ones can be made, so an operator can retire an integration without
    -- breaking the workspaces already leaning on it.
    retired_at   timestamptz,

    created_by   uuid references users (id) on delete set null,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),

    unique (workspace_id, slug),

    -- An override needs a base; a standalone must not carry one.
    check ((kind = 'override') = (base_skill_id is not null))
);

create index skills_workspace_idx on skills (workspace_id);

-- One override per base per workspace. Two would both apply to the same skill
-- with nothing to decide which spoke last, and "last wins" is the whole of what
-- an override means.
create unique index skills_one_override_per_base_idx
    on skills (workspace_id, base_skill_id) where kind = 'override';
create index skills_base_idx on skills (base_skill_id) where base_skill_id is not null;

-- Every edit, kept. Rows are written once and never updated: what a turn was
-- given has to stay answerable after the skill has moved on, which is the whole
-- of the audit trail and the only way an eval can name what it measured.
--
-- Rolling back appends rather than repointing. If "current" could move
-- backwards there would be no way to ask what was live on a given day without
-- keeping a second history of the pointer -- so the newest version is always
-- the live one, and a rollback reads as a further version carrying the old
-- body. There is no current_version_id for the same reason: a pointer beside
-- the history is a second thing to disagree with it.
create table skill_versions (
    id           uuid primary key,
    workspace_id uuid not null references workspaces (id) on delete cascade,
    skill_id     uuid not null references skills (id) on delete cascade,
    -- Monotonic per skill, so a person can say "v3" and be understood.
    ordinal      int  not null,
    body         text not null,
    -- What changed, in the words of whoever saved it.
    note         text not null default '',

    -- For an override's version: the base version it was written against.
    -- The base moves on the operator's schedule, so this is what says whether
    -- the instructions still address the prose they were written to correct --
    -- an override going stale is silent otherwise, since nothing errors when a
    -- correction stops matching what it was correcting.
    based_on_version_id uuid references skill_versions (id) on delete set null,

    created_by   uuid references users (id) on delete set null,
    created_at   timestamptz not null default now(),

    unique (skill_id, ordinal)
);

-- A skill's live version is its highest ordinal, and this is the lookup that
-- composes a prompt.
create index skill_versions_current_idx on skill_versions (skill_id, ordinal desc);

-- Deferred because the two tables reference each other.
alter table skills
    add foreign key (forked_from_version_id) references skill_versions (id) on delete set null;

-- What a skill will be reaching for, declared with the version that needs it.
--
-- Declared, never granted. A workspace's egress rules stay the only thing that
-- opens a host; this says which ones a skill expects to reach, so the gap
-- between the two can be shown to somebody who can close it.
--
-- Per version rather than per skill, because a version that adds a host is the
-- moment worth interrupting somebody for, and the difference between the two
-- sets is how that moment is found. A version that only rewords declares the
-- same hosts and disturbs nobody.
create table skill_version_hosts (
    version_id uuid not null references skill_versions (id) on delete cascade,
    host       text not null,
    primary key (version_id, host)
);

create index skill_version_hosts_host_idx on skill_version_hosts (host);

-- Which skill's declaration opened a rule, where one did.
--
-- Null when somebody added the host themselves. Set null rather than cascading
-- when the skill goes: a host is unique per workspace, so another skill may be
-- leaning on the same rule, and withdrawing network access as a side effect of
-- deleting a skill would be a surprise in the dangerous direction.
alter table egress_rules
    add column from_skill_id uuid references skills (id) on delete set null;

-- Which skills an agent is given, and in what order they are composed.
create table agent_skills (
    workspace_id uuid not null references workspaces (id) on delete cascade,
    agent_id     uuid not null references agents (id) on delete cascade,
    skill_id     uuid not null references skills (id) on delete cascade,
    -- Null follows the skill as it is edited, which is what makes an operator's
    -- skill a way to ship a fix to everyone at once. Set pins this agent to one
    -- version, for a workspace that wants changes to stop arriving unreviewed.
    version_id   uuid references skill_versions (id) on delete set null,
    position     int  not null default 0,
    created_at   timestamptz not null default now(),
    primary key (agent_id, skill_id)
);

create index agent_skills_skill_idx on agent_skills (skill_id);

-- What a reply was actually composed from.
--
-- The binding above is policy and changes; this is the fact. It answers "which
-- turns ran v3" long after v4 landed, which is what an eval keys off and what
-- an audit asks for. An override is a skill in its own right, so it takes its
-- own row and a composition is simply the list.
--
-- Cascading, because a workspace that is deleted takes its transcripts with it
-- and this is part of one. Within a live workspace a skill is retired rather
-- than deleted, which is what keeps the record whole.
create table turn_skills (
    reply_id   uuid not null references agent_messages (id) on delete cascade,
    skill_id   uuid not null references skills (id) on delete cascade,
    version_id uuid not null references skill_versions (id) on delete cascade,
    position   int  not null,
    primary key (reply_id, skill_id)
);

create index turn_skills_version_idx on turn_skills (version_id);

-- Role templates ---------------------------------------------------------------

-- What a workspace's roles start as.
--
-- Rows rather than constants in code, because which bundles exist and what they
-- are called is a deployment's business: an operator serving one trade ships
-- different names than one serving another, and changing them should not mean a
-- rebuild.
--
-- Copied into a workspace's own roles when it is created, and never consulted
-- again. Editing a template does not reach back into workspaces that already
-- copied it, the same way editing a role does not reach into the grants that
-- already name it -- a workspace's roles are its own from the moment it has any.
create table role_templates (
    name        text primary key,
    description text        not null default '',
    -- The order they are created in, so every workspace's list reads alike.
    position    int         not null default 0,
    created_at  timestamptz not null default now()
);

create table role_template_authorities (
    template_name text not null references role_templates (name) on delete cascade,
    -- Validated against the Authority enum in code, as role_authorities is: the
    -- vocabulary changes with the code, and a check constraint here would need a
    -- migration every time it did. One the code does not know is dropped when it
    -- is copied, and said so in the log rather than silently narrowing a role.
    authority     text not null,
    primary key (template_name, authority)
);

insert into role_templates (name, description, position) values
    ('admin', 'Runs the workspace: people, roles, agents, settings and files.', 0),
    ('operator', 'Builds and runs agents, and works with their files.', 1),
    ('viewer', 'Reads conversations, agents, settings and files without changing them.', 2);

insert into role_template_authorities (template_name, authority) values
    ('admin', 'users:create'), ('admin', 'users:read'), ('admin', 'users:update'),
    ('admin', 'users:delete'), ('admin', 'roles:assign'), ('admin', 'roles:manage'),
    ('admin', 'agents:create'), ('admin', 'agents:read'), ('admin', 'agents:update'),
    ('admin', 'agents:delete'), ('admin', 'sessions:create'), ('admin', 'sessions:read'),
    ('admin', 'sessions:update'), ('admin', 'sessions:delete'), ('admin', 'settings:read'), ('admin', 'settings:update'),
    ('admin', 'storage:workspace:read'), ('admin', 'storage:workspace:write'),
    ('admin', 'storage:agent:read'), ('admin', 'storage:agent:write'),
    ('admin', 'skills:read'), ('admin', 'skills:write'),
    ('admin', 'gateway:invoke'), ('admin', 'usage:read'),

    ('operator', 'agents:create'), ('operator', 'agents:read'), ('operator', 'agents:update'),
    ('operator', 'sessions:create'), ('operator', 'sessions:read'),
    ('operator', 'sessions:update'),
    ('operator', 'storage:workspace:read'), ('operator', 'storage:agent:read'),
    ('operator', 'storage:agent:write'),
    ('operator', 'skills:read'), ('operator', 'skills:write'),
    ('operator', 'gateway:invoke'),

    ('viewer', 'agents:read'), ('viewer', 'sessions:read'), ('viewer', 'settings:read'),
    ('viewer', 'storage:workspace:read'), ('viewer', 'storage:agent:read'),
    ('viewer', 'skills:read');

-- Seed roles -------------------------------------------------------------------

-- Every workspace that exists when this runs gets the templates copied, which is
-- the platform workspace and nothing else. Every workspace made afterwards is
-- copied the same way by the code that creates it, reading the same rows.
insert into roles (workspace_id, id, name, description)
select w.id, gen_random_uuid(), t.name, t.description
from workspaces w
cross join role_templates t;

insert into role_authorities (workspace_id, role_id, authority)
select r.workspace_id, r.id, a.authority
from roles r
join role_template_authorities a on a.template_name = r.name;
