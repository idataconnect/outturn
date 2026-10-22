-- Foreign keys with no index on the referencing side ---------------------------------

-- A delete of the row a foreign key points at looks for the rows pointing at
-- it, and without an index that is a scan of the referencing table, whatever
-- the `on delete` says. Partial where most rows are null, as in 0024. In a
-- transaction, unlike 0061 and 0063: none of these tables takes a write per
-- token, and the builds are short.

-- Deleting an account sets these to null.
create index agent_sessions_user_idx on agent_sessions (user_id) where user_id is not null;
create index action_items_resolved_by_idx on action_items (resolved_by)
    where resolved_by is not null;
create index triggers_owner_idx on triggers (owner_id) where owner_id is not null;
create index triggers_created_by_idx on triggers (created_by) where created_by is not null;
create index skills_created_by_idx on skills (created_by) where created_by is not null;
create index skill_versions_created_by_idx on skill_versions (created_by)
    where created_by is not null;

-- Every firing of a trigger with a concurrency ceiling counts its unfinished
-- turns through the sessions it started, under `for update` on the trigger
-- (`trigger::over_ceiling`), so a scan here holds the lock for as long as it
-- takes. Deleting a trigger sets these to null as well.
create index agent_sessions_trigger_idx on agent_sessions (trigger_id)
    where trigger_id is not null;

-- `on delete restrict`: revoking a credential for good is refused while a rule
-- or an account names it, and finding out is the scan.
create index egress_rules_credential_idx on egress_rules (credential_id)
    where credential_id is not null;
create index provider_accounts_credential_idx on provider_accounts (credential_id)
    where credential_id is not null;
