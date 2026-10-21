-- Auto-approval: a yes given in advance. See docs/auto-approval.md.

-- What an operation's declaration says a policy may do with it ----------------

-- Beside the gate it qualifies, written at publish from the same frontmatter
-- block. A version published before these keys existed reads as the doc says
-- absence does: `high`, `allowed`, nothing comparable and nothing free -- so a
-- policy can reach such a gate only by selecting it by act or by operation, only
-- with equality tests, and only for a request carrying nothing but its bound
-- fields.
alter table skill_version_gates
    add column risk    text   not null default 'high'
        check (risk in ('low', 'medium', 'high')),
    add column auto    text   not null default 'allowed'
        check (auto in ('allowed', 'never')),
    add column numbers text[] not null default '{}',
    add column free    text[] not null default '{}';

-- The policies -----------------------------------------------------------------

-- Rows a workspace can read, each naming who set it. Never deleted: revoking is
-- a column, because the records below name the policy that let each request out
-- and a record whose policy vanished explains nothing.
create table approval_policies (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    id           uuid        not null,

    -- Null: every agent in the workspace.
    agent_id     uuid                 references agents (id) on delete cascade,

    -- An inclusion lets requests through; an exclusion stops any inclusion from
    -- doing so, however broad, so no ordering question can let a narrower
    -- inclusion slip past a wider exclusion.
    effect       text        not null default 'include'
        check (effect in ('include', 'exclude')),

    -- What it covers: everything up to a risk, every gate of one act, or one
    -- operation -- a gate by its skill and its `matches`.
    selector     text        not null
        check (selector in ('risk', 'act', 'operation')),
    max_risk     text        check (max_risk in ('low', 'medium', 'high')),
    requires     text,
    skill_id     uuid,
    matches      text,

    -- A conjunction of tests on bound fields; `egress::policy::Test`.
    conditions   jsonb       not null default '[]',

    -- Caps, because a condition is per request: four hundred charges of 4999
    -- each satisfy `amount_pence <= 5000`. Per turn and per day are required.
    per_turn     integer     not null check (per_turn > 0),
    per_day      integer     not null check (per_day > 0),
    sum_field    text,
    sum_per_day  bigint      check (sum_per_day >= 0),

    -- `trial` still asks a person, and says on the question that this would
    -- have said yes. Never switched on by a threshold: somebody chooses to.
    mode         text        not null default 'trial'
        check (mode in ('trial', 'on')),

    -- Bumped by every edit. A turn commits to the version it was prepared with,
    -- and the gateway records against that version only, so an edit reaches a
    -- running turn as a person being asked.
    version      integer     not null default 1,

    note         text,

    -- Who set it, which is half of what the record says. A policy whose setter
    -- has left the workspace lapses; see docs/auto-approval.md.
    created_by   uuid                 references users (id) on delete set null,
    created_at   timestamptz not null default now(),
    updated_by   uuid                 references users (id) on delete set null,
    updated_at   timestamptz not null default now(),

    -- A risk or act selector reaches gates published after it was set. Those
    -- are covered in trial until somebody accepts them, which moves this on.
    accepted_by  uuid                 references users (id) on delete set null,
    accepted_at  timestamptz not null default now(),

    -- Required: nothing stands for ever. Renewed by editing.
    expires_at   timestamptz not null,
    revoked_by   uuid                 references users (id) on delete set null,
    revoked_at   timestamptz,

    primary key (workspace_id, id),

    check (
        (selector = 'risk' and max_risk is not null and requires is null and skill_id is null)
        or (selector = 'act' and requires is not null and max_risk is null and skill_id is null)
        or (selector = 'operation' and skill_id is not null and matches is not null
            and requires is not null and max_risk is null)
    ),
    check ((sum_field is null) = (sum_per_day is null))
);

create index approval_policies_live_idx
    on approval_policies (workspace_id)
    where revoked_at is null;

-- The record of each automatic yes --------------------------------------------

-- Written by the gateway, before the request goes out, with an insert that only
-- succeeds while the policy is on, unrevoked, unexpired and within its caps. So
-- the row is the check as well as the record: no row, no request.
--
-- No foreign key to the session or the job. A request nobody saw must stay
-- accounted for after the conversation it was made in is gone.
create table auto_approvals (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    id           uuid        not null,
    policy_id    uuid        not null,
    session_id   uuid        not null,
    job_id       uuid        not null,
    requires     text        not null,
    method       text        not null,
    host         text        not null,
    path         text        not null,
    -- The bound fields' values: what the policy tested. Not the body.
    bound        jsonb       not null,
    -- The summed field's value, where the policy sums one.
    amount       bigint,
    at           timestamptz not null default now(),

    primary key (workspace_id, id),
    foreign key (workspace_id, policy_id)
        references approval_policies (workspace_id, id)
);

-- What every cap counts: one policy's records, by turn and by time.
create index auto_approvals_policy_idx on auto_approvals (workspace_id, policy_id, at);
create index auto_approvals_job_idx on auto_approvals (workspace_id, policy_id, job_id);

-- Who may set one -------------------------------------------------------------

-- `approvals:delegate`, in the admin template and never alone: setting a policy
-- also requires `approvals:answer`, because nobody may delegate an answer they
-- could not give. Backfilled as approvals:answer was, by name and only where the
-- role still means what the template meant.
insert into role_template_authorities (template_name, authority) values
    ('admin', 'approvals:delegate');

insert into role_authorities (workspace_id, role_id, authority)
select r.workspace_id, r.id, a.authority
from roles r
join role_templates t on t.name = r.name and t.description = r.description
join role_template_authorities a on a.template_name = t.name
where a.authority = 'approvals:delegate'
on conflict do nothing;
