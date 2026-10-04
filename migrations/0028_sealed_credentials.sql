-- Sealed credentials: a secret encrypted in the browser to the gateway's
-- public key, with where it may go as the associated data. Stored here, where
-- anyone may read it and nobody but the gateway can open it. See
-- docs/sealed-credentials.md.
create table credentials (
    id uuid primary key,
    workspace_id uuid not null references workspaces(id) on delete cascade,
    name text not null,
    -- The exact bytes the seal was made under. Bytes, not jsonb: the tag is
    -- over these, and a re-serialised copy that differed by a space would
    -- make a valid seal stop opening.
    binding bytea not null,
    -- HPKE's encapsulated key followed by the ciphertext. Wiped, not the row
    -- deleted, when a credential is revoked: the rules that named it and the
    -- history of who sealed it still have something to point at.
    sealed bytea,
    -- Which of the gateway's keys it was sealed to, so two can be held during
    -- a rotation.
    key_id text not null,
    -- Bumped on every rotation and revocation. The gateway's cache inserts a
    -- read only if this is what it was when the read began.
    generation bigint not null default 1,
    created_by uuid,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    revoked_at timestamptz,
    check ((revoked_at is null) = (sealed is not null))
);

create index credentials_workspace_idx on credentials (workspace_id, id);

-- The gateway hears about a change here and drops what it holds for that id.
create function credentials_notify() returns trigger language plpgsql as $$
begin
    perform pg_notify('credentials_changed', coalesce(new.id, old.id)::text);
    return null;
end
$$;

create trigger credentials_notify
    after update or delete on credentials
    for each row execute function credentials_notify();

-- A rule names an environment variable or a sealed credential for its header,
-- never both. The old rule said a header comes with a variable; now it comes
-- with one or the other.
alter table egress_rules
    add column credential_id uuid references credentials(id) on delete restrict;

alter table egress_rules drop constraint egress_rules_check;
alter table egress_rules add constraint egress_rules_header_has_one_source check (
    (header is null and credential_env is null and credential_id is null)
    or (header is not null and (credential_env is null) <> (credential_id is null))
);

-- Who may store, rotate and revoke a workspace's credentials, and who may see
-- that they exist. Admin only by default: a credential is the workspace's
-- standing with somebody else's API, and an operator building agents does not
-- need to be able to replace it. Backfilled as approvals:answer was, by name
-- and description, so a workspace that repurposed its admin role is left alone.
insert into role_template_authorities (template_name, authority) values
    ('admin', 'credentials:read'), ('admin', 'credentials:write');

insert into role_authorities (workspace_id, role_id, authority)
select r.workspace_id, r.id, a.authority
from roles r
join role_templates t on t.name = r.name and t.description = r.description
join role_template_authorities a on a.template_name = t.name
where a.authority in ('credentials:read', 'credentials:write')
on conflict do nothing;
