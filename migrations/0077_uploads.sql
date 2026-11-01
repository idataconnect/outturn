-- Uploads a person sends in pieces. See docs/large-uploads.md.

-- One per upload: whose it is, where it lands, how large it was said to be,
-- and the store's id for the multipart upload behind it. Gone with the
-- conversation it was sent into.
create table uploads (
    id uuid primary key,
    workspace_id uuid not null references workspaces (id) on delete cascade,
    session_id uuid not null references agent_sessions (id) on delete cascade,
    user_id uuid not null references users (id) on delete cascade,
    -- The path as the person named it, under its scope, and the key it
    -- resolved to when the upload began.
    path text not null,
    key text not null,
    size bigint not null check (size > 0),
    store_upload_id text not null,
    state text not null default 'open'
        check (state in ('open', 'complete', 'abandoned')),
    created_at timestamptz not null default now(),
    -- A day, matching the bucket's rule for abandoned pieces: past it the
    -- store has dropped them, so there is nothing left to resume.
    expires_at timestamptz not null
);

create index uploads_open_by_expiry on uploads (expires_at) where state = 'open';

-- Each piece the store has accepted, with the tag completing the upload needs.
-- The store cannot be asked for these by the client this uses, so they are
-- kept as they arrive; completing hands them back, and the store refuses a
-- set that is not what it holds.
create table upload_parts (
    upload_id uuid not null references uploads (id) on delete cascade,
    number integer not null check (number >= 1),
    size bigint not null check (size > 0),
    etag text not null,
    primary key (upload_id, number)
);
