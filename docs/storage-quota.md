# Storage quota

Designed, not built. How many bytes a workspace may hold in files, and how
that is counted.

## The problem

One turn is bounded: `max_written_bytes` (default 1 GiB) is counted by the
runtime before each write. Many turns are not. A scheduled agent writing
100 MiB a run stays inside its limit every time and holds a terabyte within a
year. Nothing counts what a workspace holds, so nothing can refuse it, show it
or bill it.

## What is counted

Every object under a workspace's three prefixes (`workspaces/`, `agents/`,
`sessions/`, see [storage.md](storage.md)), whoever wrote it: an upload
through the files API, a guest's write and the text extracted from either.
Skill versions are left out. They are content-addressed and shared, and the
skills API already bounds a package's size.

## Where the count lives

A table, one row per object, and a total kept beside it:

```sql
create table stored_objects (
    key          text primary key,
    workspace_id uuid not null references workspaces on delete cascade,
    bytes        bigint not null,
    written_at   timestamptz not null default now()
);
-- workspaces.stored_bytes bigint not null default 0, kept by a trigger
-- on stored_objects insert, update and delete.
```

A row per object rather than a bare counter, because the writes that change
the total are not purely additive. Overwriting a 10 MiB file with a 1 MiB one
lowers the total by 9 MiB. Only a per-key size records that without a
`stat` before every write. The trigger keeps the total exact under concurrent
turns, without a read-modify-write in application code.

## Who records a write

The runtime writes to the bucket itself and holds no database. It already
reports each write to the API as `ExecuteEvent::Wrote { path, key }`, which is
how extraction learns of it. That event gains the object's size:

- `Wrote { path, key, bytes }`: the API upserts `stored_objects`.
- A new `Deleted { key }`: the API deletes the row. Today a guest's delete is
  not reported at all.

The files API, extraction and the storage sweep are in the API already, and
write the same rows directly. A sweep that removes a prefix deletes its rows
in the same pass.

## Who enforces it

The API, when it hands out a turn. The work it sends gains
`remaining_bytes`: the workspace's quota minus `stored_bytes`. The runtime
takes the smaller of that and `max_written_bytes` as its per-turn limit,
counted exactly as today, and refuses the write that would cross it with a
message the model can act on: the workspace's storage is full.

The files API checks the same total before accepting an upload and answers
`413` with code `too_large` when the upload would cross it.

Two turns starting together may each be handed the same remaining allowance
and overrun the quota by up to one turn's writes. That is accepted: the
quota is a budget, not a hard boundary. The alternative is reserving bytes
for each turn and handing back what it left unused, which would make every
turn's start and end a contended write on the workspace row.

## Not a security boundary

A compromised runtime holds bucket credentials and can write without
reporting it. The quota does not defend against that; nothing reachable from
the runtime could. What it defends against is honest growth, the kind
[storage.md](storage.md) calls "a bill that grows without a decision ever
being made." Under-reporting is caught afterwards by reconciliation.

## Reconciliation

Weekly, the storage sweep lists each workspace's prefixes and replaces its
rows with what the bucket actually holds. That corrects events lost to a
runtime that died mid-turn, a write that failed after it was counted, and
anything written around the reporting. Each correction is logged with the
difference, so a runtime under-reporting shows up as a pattern rather than
as noise.

The first reconciliation is also the backfill. Until it has run for a
workspace, that workspace has no quota enforced, rather than a wrong one.

## Setting the quota

A setting, `max_stored_bytes`, in the cascade as any other
([settings.md](settings.md)): a platform default, overridable per workspace
by the platform operator only. It is not overridable by the workspace itself,
since it is what the workspace is charged for. `0` means no limit, as for the
per-turn limits. Default: no limit, so an upgrade changes nothing until an
operator sets one.

The workspace's usage page shows stored bytes beside the quota.

## Open questions

- Whether session scratch counts the same as workspace files. It is swept
  after 30 idle days, so it is cheaper to hold. Counting it equally is
  simpler and is the starting point.
- What happens to a workspace already over a newly set quota. As designed,
  its turns can still read and delete but not write, which is probably right
  but should be visible in the UI before an operator sets it.
