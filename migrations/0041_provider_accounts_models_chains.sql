-- Where requests go, what is asked for there, and in what order: the three
-- objects of docs/models.md, replacing traffic_routes, which held all three in
-- one row. Each is owned by the platform (a null workspace) or by a workspace.

-- A protocol, a base URL and a credential. Two models at one provider share
-- one of these, and so one key.
create table provider_accounts (
    id             uuid        primary key,
    workspace_id   uuid        references workspaces (id) on delete cascade,
    name           text        not null,
    protocol       text        not null check (protocol in ('openai', 'anthropic', 'gemini', 'bedrock')),
    base_url       text        not null,
    -- The name of a variable in the gateway's environment, or a sealed
    -- credential bound to this account's host -- never both, and never the
    -- key itself. Neither is an endpoint that needs none, such as ollama.
    credential_env text,
    credential_id  uuid        references credentials (id) on delete restrict,
    created_at     timestamptz not null default now(),
    check (credential_env is null or credential_id is null)
);

-- A model at an account. `model` is what the account is asked for; the
-- catalog link and `facts` say what it accepts. Facts are copied from the
-- catalog when the model is linked, so a newer catalog never changes a model
-- under anyone and one that leaves the catalog keeps what it had. A model
-- with no link is hand-entered, and its facts are whatever was typed, or
-- nothing, in which case the protocol's defaults apply.
create table models (
    id               uuid        primary key,
    workspace_id     uuid        references workspaces (id) on delete cascade,
    account_id       uuid        not null references provider_accounts (id) on delete restrict,
    name             text        not null,
    model            text        not null,
    catalog_provider text,
    catalog_model    text,
    snapshot_id      uuid        references catalog_snapshots (id) on delete set null,
    facts            jsonb,
    -- Fields of the catalog entry this deployment says otherwise about.
    overrides        jsonb       not null default '{}',
    created_at       timestamptz not null default now(),
    check ((catalog_provider is null) = (catalog_model is null))
);

create index models_account_idx on models (account_id);

-- An ordered list of models, tried in turn.
create table chains (
    id           uuid        primary key,
    workspace_id uuid        references workspaces (id) on delete cascade,
    name         text        not null,
    created_at   timestamptz not null default now()
);

create table chain_links (
    chain_id uuid not null references chains (id) on delete cascade,
    position int  not null,
    model_id uuid not null references models (id) on delete restrict,
    primary key (chain_id, position)
);

-- Which chain a class of traffic walks. A workspace's row replaces the
-- platform's for that class, as a workspace's routes replaced the system
-- defaults. Surrogate key and two partial indexes for the reason given on
-- traffic_routes: the platform's rows are the ones whose workspace is null.
create table chain_assignments (
    id           uuid primary key,
    workspace_id uuid references workspaces (id) on delete cascade,
    traffic_type text not null,
    chain_id     uuid not null references chains (id) on delete restrict
);

create unique index chain_assignments_workspace_idx
    on chain_assignments (workspace_id, traffic_type) where workspace_id is not null;
create unique index chain_assignments_platform_idx
    on chain_assignments (traffic_type) where workspace_id is null;

-- From traffic_routes: each distinct endpoint and credential an account, each
-- enabled row a model, each class's rows a chain in priority order. A
-- disabled row was configured to be passed over, and is; the table is kept,
-- unread, for a release, so nothing is lost by it.
create temporary table route_accounts on commit drop as
select uuidv7() as id, workspace_id, provider, base_url, credential_ref
from (
    select distinct workspace_id, provider, base_url, credential_ref
    from traffic_routes where enabled
) d;

insert into provider_accounts (id, workspace_id, name, protocol, base_url, credential_env)
select id, workspace_id, provider || ' at ' || base_url, provider, base_url, credential_ref
from route_accounts;

create temporary table route_models on commit drop as
select uuidv7() as id, r.id as route_id, r.workspace_id, r.traffic_type, r.priority,
       r.model, a.id as account_id
from traffic_routes r
join route_accounts a
  on a.workspace_id is not distinct from r.workspace_id
 and a.provider = r.provider
 and a.base_url = r.base_url
 and a.credential_ref is not distinct from r.credential_ref
where r.enabled;

insert into models (id, workspace_id, account_id, name, model)
select id, workspace_id, account_id, model, model from route_models;

create temporary table route_chains on commit drop as
select uuidv7() as id, workspace_id, traffic_type
from (select distinct workspace_id, traffic_type from route_models) d;

insert into chains (id, workspace_id, name)
select id, workspace_id, traffic_type from route_chains;

insert into chain_links (chain_id, position, model_id)
select c.id, row_number() over (partition by c.id order by m.priority) - 1, m.id
from route_models m
join route_chains c
  on c.workspace_id is not distinct from m.workspace_id
 and c.traffic_type = m.traffic_type;

insert into chain_assignments (id, workspace_id, traffic_type, chain_id)
select uuidv7(), workspace_id, traffic_type, id from route_chains;
