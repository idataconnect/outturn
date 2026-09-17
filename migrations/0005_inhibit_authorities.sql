-- Who may stop things.
--
-- Two authorities rather than one, because the blast radius differs by an order
-- of magnitude. Stopping one misbehaving agent is the ordinary work of whoever
-- builds agents; halting everything a workspace runs is not, and a role that
-- conflated them would force the second on everyone who needed the first.
--
-- Neither is folded into `workspaces:update` or `agents:update`. A credential
-- that exists to halt an org -- a customer's spend watchdog -- should not also
-- be able to rename or delete it, and the admin UI should not carry kill rights
-- on every request it makes.

insert into role_template_authorities (template_name, authority) values
    ('admin', 'workspaces:inhibit'),
    ('admin', 'agents:inhibit'),
    -- An operator builds and runs agents, so stopping one is theirs. The
    -- workspace switch is not: it stops work they may know nothing about.
    ('operator', 'agents:inhibit');

-- Workspaces that already exist have their own copies of the templates, which
-- were made when they were created and do not track later edits. Backfilled by
-- name so a deployment upgrading into this gets the same grants a new workspace
-- would -- and only where the role still means what the template meant, which
-- is what matching on the description checks.
insert into role_authorities (workspace_id, role_id, authority)
select r.workspace_id, r.id, a.authority
from roles r
join role_templates t on t.name = r.name and t.description = r.description
join role_template_authorities a on a.template_name = t.name
where a.authority in ('workspaces:inhibit', 'agents:inhibit')
on conflict do nothing;
