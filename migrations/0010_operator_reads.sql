-- What an operator may read.
--
-- Two omissions from the original templates, found by signing in as one.
--
-- `settings:read` was the plain bug: a viewer had it and an operator did not,
-- which made operator the only role that was not a superset of the one below
-- it. Nothing justifies "the person who builds agents may not read the
-- settings those agents inherit, but the person who only looks may" -- and the
-- settings cascade is exactly what an operator needs to see to understand why
-- an agent behaves as it does.
--
-- `usage:read` is a judgement rather than a bug. An operator already holds
-- `gateway:invoke`, which is to say they are the ones spending the money; the
-- usage window is how anybody finds out that an agent deployed yesterday is
-- looping and burning tokens. Withholding it left the person best placed to
-- notice runaway spend as the one who could not see it. It is read-only and
-- scoped to their own workspace, so it discloses nothing about another tenant.
--
-- Not `settings:update`. Reading the cascade to understand an agent and
-- changing what every agent in the workspace inherits are different acts, and
-- only the first is an operator's.

insert into role_template_authorities (template_name, authority) values
    ('operator', 'settings:read'),
    ('operator', 'usage:read');

-- Workspaces that already exist have their own copies of the templates, which
-- were made when they were created and do not track later edits. Backfilled by
-- name so a deployment upgrading into this gets the same grants a new
-- workspace would -- and only where the role still means what the template
-- meant, which is what matching on the description checks. A workspace that
-- has edited its operator role has said what it wants that role to be, and
-- this must not talk over it.
insert into role_authorities (workspace_id, role_id, authority)
select r.workspace_id, r.id, a.authority
from roles r
join role_templates t on t.name = r.name and t.description = r.description
join role_template_authorities a on a.template_name = t.name
where a.authority in ('settings:read', 'usage:read')
  and t.name = 'operator'
on conflict do nothing;
