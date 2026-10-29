-- Objects forgotten from a workspace's count, and when, so the weekly listing
-- does not count again a file deleted after it listed the bucket and before
-- it wrote what it found. See docs/storage-quota.md, *Reconciliation*.
--
-- Only as long as a listing can take: a day, cleared by the listing itself.
create table stored_forgets (
    key          text        primary key,
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    forgotten_at timestamptz not null default now()
);

create index stored_forgets_at_idx on stored_forgets (forgotten_at);
