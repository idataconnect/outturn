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

-- The conversation itself. Ordered by `seq` within a session rather than by
-- timestamp, so concurrent inserts cannot interleave ambiguously.
create table agent_messages (
    id           uuid        primary key,
    session_id   uuid        not null references agent_sessions (id) on delete cascade,
    seq          bigint      not null,
    role         text        not null check (role in ('system', 'user', 'assistant', 'tool')),
    content      text        not null,

    -- Usage attribution. Null until a provider reports it; the model is
    -- recorded per message because an agent's model may change between turns.
    model        text,
    prompt_tokens     int,
    completion_tokens int,

    created_at   timestamptz not null default now(),
    unique (session_id, seq)
);

create index agent_messages_session_idx on agent_messages (session_id, seq);
