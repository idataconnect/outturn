-- Who may answer an approval an agent is waiting on.
--
-- A third authority beside the two kill switches, and not folded into either.
-- Stopping work and letting it proceed are opposite acts: somebody trusted to
-- halt a misbehaving agent is not thereby the right person to authorise a
-- payment, and an operator who builds agents is exactly who should *not* be
-- able to approve the money their own agent is about to move.
--
-- See docs/approvals.md. It answers only the first of the two questions there
-- -- may this person answer approvals at all -- because the second, whether
-- they are entitled to the thing approved, belongs to whoever owns that thing.
-- For a charge in somebody's payment API that is not ours to check: the
-- authority vocabulary is fixed and code-defined, and a customer's idea of who
-- may take money was never in it.
insert into role_template_authorities (template_name, authority) values
    ('admin', 'approvals:answer');

-- Deliberately not `operator`. They build and run the agents that raise these,
-- and a workspace that wants its operators approving can add it to the role --
-- which is the point of roles being the workspace's own. The default should not
-- be the one where the person who wrote the agent signs off its charges.

-- Workspaces that already exist hold copies of the templates from when they
-- were created, which do not track later edits. Backfilled by name, and only
-- where the role still means what the template meant -- matching the
-- description is what checks that, so a workspace that renamed or repurposed
-- its admin role is left alone rather than quietly given a new power.
insert into role_authorities (workspace_id, role_id, authority)
select r.workspace_id, r.id, a.authority
from roles r
join role_templates t on t.name = r.name and t.description = r.description
join role_template_authorities a on a.template_name = t.name
where a.authority = 'approvals:answer'
on conflict do nothing;
