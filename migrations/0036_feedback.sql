-- A person's verdict on a reply, or on a whole conversation. See
-- docs/skill-evaluation.md, which needs it twice: as a correction, and as the
-- only labeling that will ever happen.
--
-- `message_id` null is the conversation as a whole. One verdict per person per
-- target, changed in place and withdrawn by deleting it: a history of somebody
-- changing their mind is not what evaluation reads.
create table feedback (
    id           uuid        primary key,
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    session_id   uuid        not null references agent_sessions (id) on delete cascade,
    message_id   uuid        references agent_messages (id) on delete cascade,
    user_id      uuid        not null references users (id) on delete cascade,
    verdict      text        not null check (verdict in ('up', 'down')),
    note         text        not null default '',
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

-- The null message is the whole conversation, which a plain unique index would
-- let one person rate any number of times.
create unique index feedback_one_per_person_idx
    on feedback (session_id, coalesce(message_id, '00000000-0000-0000-0000-000000000000'), user_id);

-- What evaluation reads: a workspace's verdicts, newest first.
create index feedback_workspace_idx on feedback (workspace_id, created_at desc);
