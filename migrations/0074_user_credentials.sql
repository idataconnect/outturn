-- Connections that are the asker's rather than the agent's. See
-- docs/user-credentials.md.

-- Whose credential a rule's requests carry. `agent`, as every rule did before
-- this: the one the rule names, on every turn. `asker`: the credential the
-- person who sent the turn's prompt connected for this rule, resolved when the
-- turn is minted. An asker's rule may still name a credential of the
-- workspace's, used only on a turn nobody sent; naming none refuses such a
-- turn the host.
alter table egress_rules
    add column owner text not null default 'agent' check (owner in ('agent', 'asker'));

-- The one source of a header's value, restated for the second owner. An
-- agent's rule is as before. An asker's names a header for the person's key to
-- travel in, never an environment variable -- a variable is the operator's and
-- nobody's own -- and never an exchange, which is the platform authenticating
-- as itself.
alter table egress_rules drop constraint egress_rules_header_has_one_source;
alter table egress_rules add constraint egress_rules_header_has_one_source check (
    (owner = 'agent' and (
        (header is null and credential_env is null and credential_id is null)
        or (header is not null and (credential_env is null) <> (credential_id is null))
    ))
    or (owner = 'asker' and header is not null and credential_env is null
        and token_url is null)
);

-- A person's own credential, connected for one rule. The binding names the
-- person too, inside the seal; these columns are for finding it, and the seal
-- is the truth. Gone with the person and with the rule: nothing else can use
-- it, and a key nobody can use is a key better not kept.
alter table credentials
    add column user_id uuid references users (id) on delete cascade,
    add column rule_id uuid references egress_rules (id) on delete cascade,
    add check ((user_id is null) = (rule_id is null));

-- One live connection per person per rule: the one a turn resolves to.
create unique index credentials_user_rule_live_idx
    on credentials (rule_id, user_id) where revoked_at is null;

-- Names are an administrator's way of telling a workspace's keys apart, so
-- unique among those. A person's own is named for its host, and two people
-- connecting the same host is the point, not a clash.
drop index credentials_name_unique;
create unique index credentials_name_unique
    on credentials (workspace_id, name) nulls not distinct
    where revoked_at is null and user_id is null;

-- Leaving a workspace revokes every key the person connected in it. A trigger
-- rather than a line in the route, because a membership ends however its last
-- role goes -- revoked, the role deleted, the workspace's roles rewritten --
-- and a key outliving its owner's membership is one their former colleagues'
-- agents would go on using. Revoked as the route revokes, so the gateway hears
-- of it the same way.
create function credentials_revoke_on_leaving() returns trigger language plpgsql as $$
begin
    if not exists (
        select 1 from user_workspace_roles
        where user_id = old.user_id and workspace_id = old.workspace_id
    ) then
        update credentials
        set sealed = null, revoked_at = now(), generation = generation + 1,
            updated_at = now()
        where user_id = old.user_id and workspace_id = old.workspace_id
          and revoked_at is null;
    end if;
    return null;
end
$$;

create trigger credentials_revoke_on_leaving
    after delete on user_workspace_roles
    for each row execute function credentials_revoke_on_leaving();

-- Whose turn a held request was refused on: the person whose key it goes out
-- with. A held request for a host whose connection is each person's own is
-- sent only on a turn of theirs, never on whichever turn in the session comes
-- next -- which would send it with somebody else's key, or the workspace's.
-- Null on a turn nobody sent. No reference: a person removed takes their keys
-- with them, and the request then has nothing to go out with, which is what
-- it should have.
alter table held_requests add column asker uuid;
