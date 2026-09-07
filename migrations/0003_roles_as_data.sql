-- Roles become the tenant's to define.
--
-- Authorities are the fixed vocabulary in code; a role bundles them, and
-- which bundles exist and what they are called is a tenant's business. Every
-- tenant starts with copies of the defaults the code used to hard-code, and
-- may edit them from there.
--
-- Tenant-scoped on every row, with composite keys, so a role's authority row
-- cannot point at another tenant's role and row-level security can be
-- switched on later with a policy rather than a rewrite. The platform's own
-- roles (system_admin, runtime, turn) are not rows: they stay in code, so this
-- table holds one kind of thing with one owner.

create table roles (
    tenant_id   uuid        not null references tenants (id) on delete cascade,
    id          uuid        not null,
    name        text        not null,
    description text        not null default '',
    created_at  timestamptz not null default now(),
    primary key (tenant_id, id),
    unique (tenant_id, name)
);

create table role_authorities (
    tenant_id  uuid not null,
    role_id    uuid not null,
    -- Validated against the Authority enum in code on write, not here: the
    -- vocabulary changes with the code, and a check constraint would need a
    -- migration every time it did.
    authority  text not null,
    primary key (tenant_id, role_id, authority),
    foreign key (tenant_id, role_id) references roles (tenant_id, id) on delete cascade
);

-- Seed every existing tenant with the three roles the code used to define,
-- with the authorities they used to carry, so nobody's access changes.
insert into roles (tenant_id, id, name, description)
select t.id, gen_random_uuid(), r.name, r.description
from tenants t
cross join (values
    ('admin',    'Runs the workspace: people, roles, agents, settings and files.'),
    ('operator', 'Builds and runs agents, and works with their files.'),
    ('viewer',   'Reads conversations, agents, settings and files without changing them.')
) as r (name, description);

insert into role_authorities (tenant_id, role_id, authority)
select r.tenant_id, r.id, a.authority
from roles r
join (values
    ('admin', 'users:create'), ('admin', 'users:read'), ('admin', 'users:update'),
    ('admin', 'users:delete'), ('admin', 'roles:assign'), ('admin', 'roles:manage'),
    ('admin', 'agents:create'), ('admin', 'agents:read'), ('admin', 'agents:update'),
    ('admin', 'agents:delete'), ('admin', 'sessions:create'), ('admin', 'sessions:read'),
    ('admin', 'sessions:delete'), ('admin', 'settings:read'), ('admin', 'settings:update'),
    ('admin', 'storage:tenant:read'), ('admin', 'storage:tenant:write'),
    ('admin', 'storage:agent:read'), ('admin', 'storage:agent:write'), ('admin', 'gateway:invoke'),

    ('operator', 'agents:create'), ('operator', 'agents:read'), ('operator', 'agents:update'),
    ('operator', 'sessions:create'), ('operator', 'sessions:read'),
    ('operator', 'storage:tenant:read'), ('operator', 'storage:agent:read'),
    ('operator', 'storage:agent:write'), ('operator', 'gateway:invoke'),

    ('viewer', 'agents:read'), ('viewer', 'sessions:read'), ('viewer', 'settings:read'),
    ('viewer', 'storage:tenant:read'), ('viewer', 'storage:agent:read')
) as a (role_name, authority) on a.role_name = r.name;

-- Grants point at role rows rather than naming roles.
alter table user_tenant_roles add column role_id uuid;

update user_tenant_roles g
   set role_id = r.id
  from roles r
 where r.tenant_id = g.tenant_id and r.name = g.role;

-- Every grant named one of the three, so every grant now has a row. Anything
-- that did not would be a grant to a role that never existed.
delete from user_tenant_roles where role_id is null;

alter table user_tenant_roles
    drop constraint user_tenant_roles_pkey,
    drop constraint user_tenant_roles_role_check,
    drop column role,
    alter column role_id set not null,
    add primary key (user_id, tenant_id, role_id),
    add foreign key (tenant_id, role_id) references roles (tenant_id, id) on delete cascade;
