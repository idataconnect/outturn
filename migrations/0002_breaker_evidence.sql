-- What a failure is evidence of, rather than how many there have been.
--
-- `provider_health` counted failures and opened a circuit at five. That is the
-- right shape for a provider that has gone away and the wrong one for a
-- provider that is answering: one caller sending a request that makes an
-- upstream throw 500s looks identical to an outage, and takes the endpoint
-- away from everybody. The fix is not a bigger threshold, it is counting a
-- different thing -- see `gateway::breaker::policy`, which holds the reasoning
-- and the tests.

-- Which circuit a row is. An endpoint reached with the platform's own
-- credential shares a rate limit and a fate, so it gets one circuit for
-- everybody; a workspace using its own credential gets a circuit of its own,
-- or one tenant's expired key would open it for tenants whose keys are fine.
--
-- Nullable rather than a sentinel: 'platform' is the absence of a workspace,
-- and a nil uuid would be a workspace id that looks real to every join.
alter table provider_health
    add column workspace_id uuid references workspaces (id) on delete cascade;

-- The key was `endpoint` alone, which cannot hold two circuits for one host.
-- Dropped and replaced by two partial uniques for the same reason
-- `traffic_routes` has them: a primary key cannot contain nulls, and the
-- platform's rows are exactly the ones whose workspace is null.
alter table provider_health drop constraint provider_health_pkey;
alter table provider_health add column id uuid;
update provider_health set id = uuidv7() where id is null;
alter table provider_health alter column id set not null;
alter table provider_health add primary key (id);

create unique index provider_health_platform_idx
    on provider_health (endpoint) where workspace_id is null;
create unique index provider_health_workspace_idx
    on provider_health (endpoint, workspace_id) where workspace_id is not null;

-- Evidence that means nothing until it is wide.
--
-- A 500 says the service answered and failed; whether it failed at this
-- request or at everything is not knowable from one caller. So an undetermined
-- failure is remembered as a sighting rather than added to a count, and a
-- circuit opens on the number of *distinct callers* inside a window. Volume
-- from one caller cannot then impersonate breadth, which is the whole point.
--
-- One row per report rather than a counter per caller: the window moves, and
-- counting distinct callers inside it needs the times, not a total.
create table breaker_sightings (
    id            uuid        primary key,
    -- The circuit this is evidence about, matching provider_health's key.
    endpoint      text        not null,
    workspace_id  uuid        references workspaces (id) on delete cascade,
    -- Who saw it. Breadth counts workspaces for a platform circuit and
    -- sessions for a workspace one, so both are kept and the policy picks.
    --
    -- A session rather than an agent because that is what a turn token names:
    -- the gateway is never told which agent is running, and taking one on
    -- trust from the tier that runs workspace code would be believing the
    -- thing this is meant to be independent of.
    -- No foreign key on either, deliberately. This is evidence about an
    -- endpoint rather than a record about a workspace: a tenant deleted in the
    -- middle of an incident should not take the evidence with it, and -- worse
    -- -- a failing insert would drop the sighting silently and leave the
    -- circuit counting short of the breadth it actually had.
    seen_by_workspace uuid    not null,
    seen_by_session   uuid    not null,
    seen_at       timestamptz not null default now()
);

-- Counting distinct callers for one circuit inside a window is the only read,
-- and the only write is an insert. Ordered so the window is a range scan.
create index breaker_sightings_window_idx
    on breaker_sightings (endpoint, workspace_id, seen_at desc);

-- Sightings age out, and nothing else would ever delete them. Evidence older
-- than the widest window the policy uses is not evidence of anything current,
-- and a table that only grows is a table somebody meets at 3am.
create index breaker_sightings_seen_at_idx on breaker_sightings (seen_at);
