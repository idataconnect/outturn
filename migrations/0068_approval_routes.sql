-- Who is asked about a request whose risk is high, where a workspace names
-- somebody. See docs/auto-approval.md, "The same answer routes the question".
-- Roles, as every queue target is, never their members. None named, and high
-- asks whoever may answer approvals, as every risk did before.
create table approval_routes (
    workspace_id uuid        not null,
    risk         text        not null check (risk in ('high')),
    role_id      uuid        not null,
    updated_by   uuid                 references users (id) on delete set null,
    updated_at   timestamptz not null default now(),
    primary key (workspace_id, risk, role_id),
    foreign key (workspace_id, role_id) references roles (workspace_id, id) on delete cascade
);
