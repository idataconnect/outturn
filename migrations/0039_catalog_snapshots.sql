-- The model catalog, one row per version of it a deployment has held: the
-- one the release shipped, each fetch that brought something new, each file
-- it was given. Configured models will name the snapshot their facts came
-- from, so a newer catalog never changes a model under anyone, and a model
-- that leaves the catalog keeps the facts it had (docs/models.md).
create table catalog_snapshots (
    id          uuid        primary key,
    fetched_at  timestamptz not null default now(),
    -- `shipped`, the URL it was fetched from, or `file:` and a path.
    source      text        not null,
    -- The server's validator, so the next check can ask whether anything
    -- changed rather than fetch it all again.
    etag        text,
    -- Over the reduced catalog: a fetch that brought nothing new is not kept.
    sha256      text        not null,
    models      jsonb       not null
);

create index catalog_snapshots_fetched_idx on catalog_snapshots (fetched_at desc);
