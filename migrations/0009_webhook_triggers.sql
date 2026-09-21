-- Turns that start because something outside sent one.
--
-- The other half of docs/triggers.md. A schedule fires on a clock this
-- platform owns; a webhook fires when somebody else decides, which is why the
-- columns below are mostly about refusing.
create table webhook_triggers (
    id           uuid        primary key,
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    agent_id     uuid        not null references agents (id) on delete cascade,

    name         text        not null,

    -- The unguessable part of the URL this is reached at. Not a secret and
    -- never treated as one -- a URL leaks into logs, browser history,
    -- screenshots and support tickets -- but there is no reason to make it
    -- guessable either, and an endpoint nobody has the path for is one fewer
    -- thing answering unauthenticated requests.
    path         text        not null unique,

    -- Which scheme this sender uses, enforced as the only one. A delivery
    -- failing the declared check is refused rather than retried against
    -- something weaker, which is what keeps having two from meaning the weaker
    -- one is the default.
    --
    -- 'hmac' is preferred: the digest proves the body without the credential
    -- travelling. 'shared_secret' exists because Postmark and others do not
    -- sign at all, and a platform that only accepts signatures cannot receive
    -- email -- see docs/triggers.md.
    scheme       text        not null default 'hmac'
                 check (scheme in ('hmac', 'shared_secret')),

    -- What the sender signs with, or sends. Named for what it is rather than
    -- hidden: unlike an egress credential this one is compared here, so it
    -- has to be readable by the tier doing the comparing.
    --
    -- Which means anything that returns this table to a browser must not
    -- return this column, and the API does not.
    secret       text        not null,

    -- What the agent is asked, with the delivery's body substituted for
    -- {{body}}. A template rather than the raw body as the prompt: the body is
    -- text from outside, and a model reads it better as data inside an
    -- instruction than as the whole instruction. That is framing, not a
    -- defence -- the bound is the agent's own scopes and egress rules.
    prompt       text        not null,

    enabled      boolean     not null default true,

    -- The workspace's label for whose work this is, copied onto the session
    -- and from there onto the usage ledger, as a schedule's is.
    account      text,

    -- Who set it up. Kept apart from who is waiting for the reply, because
    -- nobody is: the turn's `user_id` stays null, which the ledger expects and
    -- which stops a delivery clearing a stopped session's latch.
    owner_id     uuid        references users (id) on delete set null,

    -- The ceiling, and the only thing standing between a public endpoint and a
    -- workspace's whole queue. A schedule can fire only as often as its own
    -- expression says; a hook fires as often as whoever holds the URL chooses.
    --
    -- A fixed window rather than a sliding one: it resets on the hour rather
    -- than tracking the last hour exactly, which admits up to twice the limit
    -- across a window boundary. That is deliberate. This exists to stop a
    -- runaway, not to meter billing, and a fixed window is one row and one
    -- statement where a sliding one is a row per delivery.
    max_per_hour int         not null default 60 check (max_per_hour > 0),
    window_start timestamptz not null default now(),
    window_count int         not null default 0,

    -- What happened last, for the row in the list. Until notifications exist
    -- this is the only place anybody learns a hook has been refused all week.
    last_at      timestamptz,
    last_status  text        check (last_status in ('ok', 'refused', 'failed')),
    last_error   text,
    -- How many deliveries the ceiling turned away, ever. Kept because a hook
    -- silently dropping half its traffic looks exactly like a sender that
    -- stopped sending.
    --
    -- The ceiling and nothing else. Refusals decided before the credential is
    -- proved -- a bad signature, a drifted clock, a disabled trigger -- are
    -- logged and never counted here, for two reasons. They would make this
    -- number attacker-controlled, since anybody holding the URL can produce
    -- them at will; and they would make it ambiguous, so an operator seeing it
    -- climb would raise `max_per_hour` when the sender's clock is what needs
    -- fixing.
    refused      int         not null default 0,

    created_by   uuid        references users (id) on delete set null,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

-- The inbound route's only lookup, and it happens before anything is
-- authenticated, so it must be an index hit rather than a scan.
create unique index webhook_triggers_path_idx on webhook_triggers (path);

create index webhook_triggers_agent_idx on webhook_triggers (workspace_id, agent_id);

-- Which trigger started a session, where one did. Set null rather than
-- cascading, for the same reason a schedule's is: deleting the trigger should
-- not delete the record of what it did.
alter table agent_sessions
    add column webhook_trigger_id uuid references webhook_triggers (id) on delete set null;
