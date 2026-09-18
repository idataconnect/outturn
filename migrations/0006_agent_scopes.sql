-- Which agents a person's authorities apply to, where somebody has narrowed
-- them.
--
-- An authority is a workspace-wide statement: holding `sessions:read` reads
-- every conversation with every agent. Right for a workspace whose agents are
-- all one business, wrong for one running accounting beside support -- so a row
-- here says "this person's narrowed authorities apply to this agent, and not to
-- the others". See docs/authorities.md.
--
-- Per person rather than per role, because two support leads holding one role
-- may cover different agents: the job is the same and the patch differs, and a
-- role per patch is how a role list stops being readable.
--
-- Absence means no narrowing. A person with no rows here holds their
-- authorities across the whole workspace exactly as before, which is what makes
-- this opt-in and this migration a no-op: the alternative, absence meaning no
-- access, locks every existing workspace out of its own data to buy a default
-- nobody asked for.
create table user_agent_scopes (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    user_id      uuid        not null references users (id) on delete cascade,
    agent_id     uuid        not null,
    created_at   timestamptz not null default now(),
    primary key (workspace_id, user_id, agent_id),
    -- `agents` is keyed by id alone, so the reference is to that and the
    -- workspace in the key is what keeps a scope readable on its own. A row
    -- naming an agent in another workspace would be found by no query here,
    -- every one of which filters by workspace first.
    foreign key (agent_id) references agents (id) on delete cascade
);

-- Every scope a workspace has, which is what a pod caches in one entry.
create index user_agent_scopes_workspace_idx on user_agent_scopes (workspace_id);
