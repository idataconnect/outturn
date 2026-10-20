-- What a declared write did, by the idempotency key its identity derives, so
-- the same write sent again is answered rather than repeated
-- (docs/idempotency.md). `attempted` is a write sent whose answer was never
-- seen; `completed` keeps the answer. Per workspace, and for a day: long
-- enough for every retry, short enough that a deliberate repeat next week is
-- not mistaken for one.
create table tool_invocations (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    key          text        not null,
    state        text        not null check (state in ('attempted', 'completed')),
    response     jsonb,
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null,
    primary key (workspace_id, key)
);

create index tool_invocations_expiry_idx on tool_invocations (expires_at);
