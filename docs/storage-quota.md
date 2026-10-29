# Storage quota

Built; *As built*, at the end, says where. How many bytes a workspace may
hold in files, and how that is counted.

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
`413` with code `too_large` when the upload would cross it. The check and the
write are not one step, so uploads arriving together can each pass and land
past the quota by what they hold between them -- the same slack, accepted for
the same reason, as turns starting together below.

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

A setting, `max_stored_bytes`, in the cascade ([settings.md](settings.md)).
The platform default applies everywhere, and an operator may override it per
workspace, by hand or through the API, typically from a plan system's webhook
when a workspace's plan changes. outturn accounts against the quota and does
no billing.

Who else may set it is the deployment's choice, not the catalog's. A
deployment that sells plans wants it operator-only; one that sells storage by
use wants each workspace's admins to set their own. So a second, operator-only
setting, `workspace_sets_stored_bytes`, off by default, makes
`max_stored_bytes` workspace-overridable when on. The catalog's fixed
who-may-override (settings.md, *Who may override*) gains this one exception,
and it should be built so another setting can use it the same way rather
than as a special case.

`0` means no limit, as for the per-turn limits. Default: no limit, so an
upgrade changes nothing until an operator sets one.

The workspace's usage page shows stored bytes beside the quota.

## Over quota

Session scratch counts the same as workspace files: data a workspace holds is
data it is using, however soon it will be swept.

A workspace over its quota, for instance after its plan was lowered, can
still read, list and delete everything. Writes are refused, and so are
uploads. Wherever someone can start a write, the UI shows that the workspace
is over quota before they try, not only in the refusal.

Not yet: wording an operator chooses for this, such as a warning that storage
over the quota will be swept after a date. That waits on operator-editable
text ([roadmap.md](roadmap.md)), and on a sweep that would do it.

## As built

- **The count.** `stored_objects` and `workspaces.stored_bytes`, kept by a
  trigger, with `workspaces.stored_counted_at` (migration 0072). Recorded and
  forgotten by `outturn_core::storage::stored`: by the files API on upload
  and delete, by extraction for the text it writes, by the storage sweep for
  what it removes, and by the worker for what a runtime reports --
  `ExecuteEvent::Wrote` now carries `bytes`, and `ExecuteEvent::Deleted` is
  new. Text extracted from a file counts, under the file's workspace.
- **The allowance.** `ExecuteRequest::remaining_stored_bytes`, from
  `api::quota::remaining`; the runtime counts every byte written against it
  beside `max_written_bytes` (`AgentHost::take_bytes`) and refuses with
  "this workspace's storage is full". A quota that cannot be read hands the
  turn nothing to write rather than no limit.
- **Uploads** are checked by `api::quota::check` before anything is stored,
  giving back what the files replaced: `413`, code `too_large`.
- **The listing** is `storage_sweep::recount`, ten workspaces an hour, each
  every seven days; rows written after a listing began are kept, and a key
  forgotten after it began is not counted again (`stored_forgets`, migration
  0073, kept a day).
- **Settings.** `max_stored_bytes` and `stored_bytes_set_by`, both
  operator-only; the first names the second in `Setting::delegated_by`, which
  any setting can use. The operator sets one workspace's at
  `PUT /v1/workspaces/{id}/settings/{key}`.
- **The API.** `GET /v1/storage` for the caller's workspace and
  `GET /v1/workspaces/{id}/storage` for the operator: `stored_bytes`,
  `quota`, `counted`, `full`.
- **The pages.** A storage tile on the dashboard; the files panel says the
  workspace is full and disables uploading; the operator's workspace page
  sets the quota in gigabytes.
