-- Staying on a template version. See docs/agent-templates.md.
--
-- Off by default: an operator who needs every workspace on the newest version
-- -- usually for a required template -- leaves it so. A pin is honored only
-- while the template allows it, so turning this off brings every agent back
-- to the newest without touching their rows.
alter table agent_templates
    add column allow_pinning boolean not null default false;

-- The version an agent stays on. Null follows the newest, as every agent made
-- from a template did until now.
alter table agents
    add column template_version_id uuid
        references agent_template_versions (id) on delete set null;
