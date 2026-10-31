-- A rule whose requests carry, beside the agent's own credential, a statement
-- the gateway signs of who is asking. See docs/user-credentials.md, *A signed
-- on-behalf-of header, instead*.
--
-- Only on an agent's rule that has a credential: the credential is what tells
-- the backend the statement came from outturn, and an asker's rule already
-- sends the person's own key, which says more than the statement could.
alter table egress_rules
    add column on_behalf_of boolean not null default false,
    add constraint egress_rules_on_behalf_of_vouched check (
        not on_behalf_of
        or (owner = 'agent'
            and (credential_env is not null or token_url is not null
                 or credential_id is not null))
    );
