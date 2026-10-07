-- Settings a template fixes, by catalog key: a temperature the operator
-- certified, say. They win over the workspace's and the agent's own, since a
-- value a workspace could change is not one the operator fixed. Validated
-- against the catalog when a version is published. See docs/agent-templates.md.
alter table agent_template_versions
    add column settings jsonb not null default '{}'::jsonb;
