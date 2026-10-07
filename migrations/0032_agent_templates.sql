-- Agent templates: an agent an operator defines once, made in each workspace
-- that should have it. See docs/agent-templates.md.
--
-- Not under a workspace. A template is the operator's, as a platform skill is,
-- and what it is made into is an ordinary agent row in each workspace -- with
-- its own id, so its sessions, files, schedules and triggers stay inside the
-- tenant exactly as a hand-made agent's do.
create table agent_templates (
    id               uuid        primary key,
    slug             text        not null unique,
    -- required: made in every workspace, and not removable there.
    -- default:  made in every workspace, and removable by its admin.
    -- optional: offered in the catalog, made when an admin adds it.
    availability     text        not null default 'optional'
                     check (availability in ('required', 'default', 'optional')),
    -- Whether a workspace may write its own section of the prompt.
    allow_additions  boolean     not null default true,
    retired_at       timestamptz,
    created_at       timestamptz not null default now(),
    updated_at       timestamptz not null default now()
);

-- What a template says, one row per publish. Never updated: a workspace's agent
-- follows the newest, and "which version was this turn" has to stay answerable.
create table agent_template_versions (
    id            uuid        primary key,
    template_id   uuid        not null references agent_templates (id) on delete cascade,
    ordinal       int         not null,
    name          text        not null,
    description   text        not null default '',
    -- What every workspace's agent must do. No workspace changes these.
    requirements  text        not null default '',
    -- How the operator expects most businesses to work. A workspace may change
    -- these in its own section.
    defaults      text        not null default '',
    -- The requirements restated in a line, read last. Optional; see the doc.
    reminder      text        not null default '',
    -- The agent's policy, as a hand-made agent's: the model, among others.
    policy        jsonb       not null default '{}'::jsonb,
    -- Tools offered from the first round. The operator made the agent for a
    -- job and knows which it reaches for.
    eager_tools   text[]      not null default '{}',
    note          text        not null default '',
    created_by    uuid        references users (id) on delete set null,
    created_at    timestamptz not null default now(),
    unique (template_id, ordinal)
);

-- The skills a template version gives its agents. Operator skills, from the
-- platform workspace; each follows or is pinned as an agent's own binding is.
create table agent_template_skills (
    template_version_id uuid not null references agent_template_versions (id) on delete cascade,
    skill_id            uuid not null references skills (id) on delete cascade,
    version_id          uuid references skill_versions (id) on delete set null,
    position            int  not null default 0,
    primary key (template_version_id, skill_id)
);

-- Which template an agent was made from, and what its workspace added to the
-- prompt. Null for an agent made by hand, which is every agent until now.
alter table agents
    add column template_id        uuid references agent_templates (id) on delete set null,
    add column workspace_addition text not null default '';

-- One agent per template per workspace: provisioning is safe to run again.
create unique index agents_one_per_template
    on agents (workspace_id, template_id) where template_id is not null;

-- A default template's agent a workspace removed. Remembered so a later
-- publish does not put it back; adding it again clears this.
create table agent_template_dismissals (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    template_id  uuid        not null references agent_templates (id) on delete cascade,
    created_at   timestamptz not null default now(),
    primary key (workspace_id, template_id)
);
