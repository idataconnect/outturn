-- Machine users: accounts that exist to hold connections for the turns nobody
-- sent. See docs/user-credentials.md, *Nobody asked*.

-- The workspace a machine user belongs to; null for a person. One workspace,
-- and gone with it.
alter table users
    add column machine_of uuid references workspaces (id) on delete cascade;

-- Nobody signs in as one and it holds no role: it is nobody, which is why an
-- administrator may connect its keys when for a person only they may. Refused
-- here rather than in each route, so no route that adds a sign-in or a role
-- can forget to ask.
create function users_refuse_machine() returns trigger language plpgsql as $$
begin
    if exists (select 1 from users where id = new.user_id and machine_of is not null) then
        raise exception 'a machine user cannot sign in or hold a role'
            using errcode = 'check_violation';
    end if;
    return new;
end
$$;

create trigger user_identities_refuse_machine
    before insert or update on user_identities
    for each row execute function users_refuse_machine();

create trigger user_workspace_roles_refuse_machine
    before insert or update on user_workspace_roles
    for each row execute function users_refuse_machine();

-- The machine user an agent's turns nobody sent carry as their asker. One per
-- agent, because a turn has one asker: every rule whose connection is each
-- person's own resolves to the key connected for this account.
alter table agents
    add column machine_user_id uuid references users (id) on delete set null;
