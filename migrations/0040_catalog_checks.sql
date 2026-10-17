-- When this deployment last asked whether the catalog changed, and what the
-- source said it was then, so the next check is a conditional request that
-- costs the source a "not modified" when nothing has. One row.
create table catalog_checks (
    one            boolean     primary key default true check (one),
    source         text,
    etag           text,
    checked_at     timestamptz,
    -- Offset per deployment, so every install does not ask in the same minute.
    next_check_at  timestamptz not null,
    last_error     text
);
