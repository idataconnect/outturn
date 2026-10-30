# Storage

What is built, what is not, and why the obvious layout is not the one to use.

## What exists

One bucket, partitioned by prefix, laid out scope-first as the rest of this
document argues for. A guest names a file by its scope and a path --
`session/notes.md`, `agent/procedures.md`, `workspace/reference/pricing.csv` --
and `scope::resolve` maps that onto the real key for the turn's space (workspace,
agent, session), refusing anything that tries to climb out rather than
clamping it back inside: a clamped traversal reads the wrong file and reports
success, and the caller never learns its path was wrong.

The scope is a path segment rather than a separate argument because a path is
the one thing every model reliably produces. A path with no scope is answered
with a correction that shows the same path under all three, which is the one
storage error a model hits most and the one it can fix on its own. Listing
with no prefix lists all three scopes.

What an agent may do with each scope is decided above the runtime, in the
settings cascade: session is always read/write, agent is read/write by default,
and workspace is read-only by default so a prompt that talks an agent into
overwriting shared reference material finds it cannot. Either of the latter two
can be set to `none`, for an agent with no business in those files at all --
a triage agent beside an HR agent. Writing implies reading: the vocabulary has
no way to say otherwise, and a write extracts the file's text straight back
out.

A skill version's files sit under a fourth prefix, `skills/<workspace>/<sha256>`,
written through the skills API rather than the files API and never swept: a
version is immutable, and what it said has to stay readable
([skill-packages.md](skill-packages.md)). An agent reads them as
`skill/<slug>/<path>`, read-only, through the table its turn was sent rather
than by listing the prefix, which holds every version's content by hash.

Session files are swept by the API once the session has been idle for
`OUTTURN_SESSION_FILE_TTL_DAYS` (default 30) with nothing queued, parked or
waiting to wake it -- from its last activity, not from a file's age. A bucket
lifecycle rule did this before and could count only age, so a conversation
older than the window lost its uploads mid-conversation. The text read out of
those files goes with them, so a file the listing no longer shows is not still
readable.

Deleting a session, an agent or a workspace writes its prefixes into
`storage_deletions` from a trigger, which catches cascades too, and the same
hourly sweep empties them (`api::storage_sweep`). The bucket's own lifecycle
keeps only the cleanup of uploads nobody finished.

Buckets are not the partition because buckets are a limited resource: a hundred
per AWS account by default, a thousand at the ceiling. A limit on buckets would
become a limit on customers.

A guest is never told which workspace it belongs to, so it cannot name another one
and cannot construct a path into one. The host resolves every path; nothing the
component does can widen its own reach.

People reach the same three scopes through the API: `/v1/agent-sessions/{id}/files`
lists what the caller may see, and `PUT`, `GET` and `DELETE` on
`.../files/{scope}/{path}` write, fetch and remove. Session scope needs only
what being in the conversation needs; the longer-lived scopes need the storage
authorities (docs/authorities.md). The chat page's files panel is built on
these, with a select for where an upload lands that offers only the scopes the
person may write and defaults to the conversation. A file uploaded there is one
the agent lists and reads by the same name.

How much a workspace may hold in all of this -- session files and extracted
text included, skill versions not -- is its storage quota, counted per object
as files are written and deleted: [storage-quota.md](storage-quota.md).

Not yet: promotion of a file from session scope to something longer-lived,
and retention configured per workspace rather than by one variable.

## The problem this leaves

Agents run on schedules and write as they go, and nobody prunes what they
produce. Files accumulate the way a corporate inbox accumulates: not because
anyone decided to keep them, but because deleting them was never anyone's job.
Storage without a retention story is a bill that grows without a decision ever
being made.

## Scopes

Three lifetimes, which is a property of the data rather than of who wrote it:

- **Workspace** — operating procedures, reference material, anything an agent is
  expected to consult across sessions. Kept until someone deletes it.
- **Agent** — belonging to one agent's work rather than to the organization.
- **Session** — artifacts of one conversation. Overflow from a tool result too
  large to show, intermediate files, scratch. Valuable for minutes.

## Why the hierarchy is the wrong layout

The obvious arrangement puts the scope at the end, so a workspace's data is
gathered in one place:

```
workspaces/<workspace>/                          workspace-wide
workspaces/<workspace>/agents/<agent>/           one agent's
workspaces/<workspace>/agents/<agent>/sessions/<session>/
```

This is the better model of the world and it cannot carry retention.

**S3 lifecycle rules match on prefix and nothing else.** With the scope at the
end, "sessions expire after thirty days" is not expressible as a rule: it would
need wildcards in the workspace and agent positions, and prefix matching has no
wildcards. The only way to write it is one rule per agent.

**And there is a cap of 1000 lifecycle rules per bucket.** Not per workspace — per
bucket. One rule per agent exhausts it at a thousand agents across all
customers, which is not a scale to grow into but one to hit almost immediately.

Inverting it puts the retention class in the prefix, where a rule can match it:

```
workspaces/<workspace>/...                       kept until deleted
agents/<workspace>/<agent>/...                kept while the agent exists
sessions/<workspace>/<agent>/<session>/...    swept
```

Three rules for three classes, and it stays three however many workspaces there
are. The 1000-rule budget is then spent only on workspaces who need something
other than the default, which is what a budget that size is for.

The cost is real and worth naming: a workspace's data is now scattered across
three top-level prefixes rather than gathered under one. Deleting everything
for a workspace is three operations. Browsing their storage means looking in three
places. That is a worse model of the world, traded for a retention story that
works.

It is also reversible. The layout lives in `scope.rs` and nothing else encodes
it — the guest is never told the shape, and paths are resolved by the host on
every call. If retention moved entirely into a sweeper of our own, the
hierarchy could come back and only that one file would change.

## Lifecycle rules are not enough on their own

They know two things: age, and prefix. Policies people actually ask for need
more than that.

- "Keep the last N sessions" — a count, not an age.
- "Delete everything for this customer" — an obligation with a deadline, and
  one that has to be demonstrable afterwards.
- "Keep it while the session is open, sweep a week after it closes" — depends
  on state the object store cannot see.

So a sweeper is needed for anything beyond age, and the honest arrangement is
both: lifecycle rules as the backstop that keeps working when the sweeper is
broken or was never deployed, and a job for the policies that need to know
something S3 does not.

The distinction that decides how much this matters is whether retention here is
about **cost** or **obligation**. Cost tolerates a sweeper that misses a run.
Obligation does not, and needs its deletions recorded rather than merely
performed.

## Not built

Promotion of a file from session scope to something longer-lived, and
retention configured per workspace. The sweeper is built
(`api::storage_sweep`); it sweeps by idleness and deletion, not by a policy a
workspace chooses. Overflow from a tool result too large to show -- written
to session scope so the model can go and read it rather than losing it -- is
the next thing that will want the session scope that now exists.

Writing and reading objects in streams rather than whole, so a guest can
produce binaries of any size: [streaming-storage.md](streaming-storage.md).

## Rows in the database

Objects are not the only thing that accumulates. The API's reaper tick
(`spawn_reaper` in `crates/api/src/worker/sweeps.rs`) is what sweeps tables
on a schedule; each sweep is a function beside the rule it keeps, and the
tick only calls it.

| Table | Kept | Swept by |
|---|---|---|
| `events`, the streamed kinds (`chat.delta`, `.reasoning`, `.writing`, `.tool`, `.tool_result`, `.steer`) | Until the reply they name is finished, or gone, and a day besides | `chat::forget_streamed` |
| `events`, everything else | With its conversation | The session's cascade |
| `tool_invocations` | A day, by `expires_at` | `invocations::forget_expired` |
| `trigger_firings` | Thirty days from firing, by `expires_at` | `trigger::forget_old_firings` |
| `catalog_snapshots` | The newest three, and any a model names | `catalog::forget_old_snapshots` |
| `live_sessions`, refresh tokens, webhook deliveries, breaker sightings | Until they expire | Their own stores |

The streamed kinds are a reply's only record while its row is empty: the
transcript read replays them into a reply still being written, and an
interrupted one is sealed from them. So the sweep is keyed on the reply being
finished, never on age alone -- a reply that stopped and was never sealed
keeps its events however old they are. The day after finishing is for a poll
cursor from a tab left open overnight.

Kept on purpose, with nothing sweeping them:

- **Settled action items.** An item is settled rather than deleted so a queue
  that emptied can still say why, and an approval's record is what an audit
  reads. They go with their workspace.
- **Held requests.** Every turn of a conversation is told what became of each
  request held in it, for as long as the refusal is in its history, so one
  swept while its conversation lives would leave the model a stale refusal
  with nothing to explain it. They go with their item or their conversation,
  by cascade.
- **Session prompts.** Already go with their session, by cascade (0030);
  there are none of deleted sessions to sweep.

A horizon on settled items is a retention policy a workspace should choose,
not one to pick for every deployment here.
