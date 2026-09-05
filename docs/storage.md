# Storage

What is built, what is not, and why the obvious layout is not the one to use.

## What exists

One bucket, partitioned by prefix. `scope::root_for(tenant)` returns
`tenants/{uuid}/` and `scope::resolve` maps a guest's relative path onto it,
refusing anything that tries to climb out rather than clamping it back inside —
a clamped traversal reads the wrong file and reports success, and the caller
never learns its path was wrong.

Buckets are not the partition because buckets are a limited resource: a hundred
per AWS account by default, a thousand at the ceiling. A limit on buckets would
become a limit on customers.

A guest is never told which tenant it belongs to, so it cannot name another one
and cannot construct a path into one. The host resolves every path; nothing the
component does can widen its own reach.

That is the whole of it. There is no scoping below the tenant, no retention, no
sweeping, and nothing distinguishes a file an agent will need next year from a
scratch file written during one turn.

## The problem this leaves

Agents run on schedules and write as they go, and nobody prunes what they
produce. Files accumulate the way a corporate inbox accumulates: not because
anyone decided to keep them, but because deleting them was never anyone's job.
Storage without a retention story is a bill that grows without a decision ever
being made.

## Scopes

Three lifetimes, which is a property of the data rather than of who wrote it:

- **Tenant** — operating procedures, reference material, anything an agent is
  expected to consult across sessions. Kept until someone deletes it.
- **Agent** — belonging to one agent's work rather than to the organisation.
- **Session** — artifacts of one conversation. Overflow from a tool result too
  large to show, intermediate files, scratch. Valuable for minutes.

## Why the hierarchy is the wrong layout

The obvious arrangement puts the scope at the end, so a tenant's data is
gathered in one place:

```
tenants/<tenant>/                          tenant-wide
tenants/<tenant>/agents/<agent>/           one agent's
tenants/<tenant>/agents/<agent>/sessions/<session>/
```

This is the better model of the world and it cannot carry retention.

**S3 lifecycle rules match on prefix and nothing else.** With the scope at the
end, "sessions expire after thirty days" is not expressible as a rule: it would
need wildcards in the tenant and agent positions, and prefix matching has no
wildcards. The only way to write it is one rule per agent.

**And there is a cap of 1000 lifecycle rules per bucket.** Not per tenant — per
bucket. One rule per agent exhausts it at a thousand agents across all
customers, which is not a scale to grow into but one to hit almost immediately.

Inverting it puts the retention class in the prefix, where a rule can match it:

```
tenants/<tenant>/...                       kept until deleted
agents/<tenant>/<agent>/...                kept while the agent exists
sessions/<tenant>/<agent>/<session>/...    swept
```

Three rules for three classes, and it stays three however many tenants there
are. The 1000-rule budget is then spent only on tenants who need something
other than the default, which is what a budget that size is for.

The cost is real and worth naming: a tenant's data is now scattered across
three top-level prefixes rather than gathered under one. Deleting everything
for a tenant is three operations. Browsing their storage means looking in three
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

Everything below the tenant scope. Named here so nobody mistakes the list for
a description of the code: agent and session scopes, the inverted prefixes,
retention configured per agent, the sweeper, promotion of a file from session
scope to something longer-lived, and a UI choice of where an upload lands.

The first thing that will need session scope is overflow from a tool result too
large to show: written somewhere the model can go and read rather than
discarded, which turns truncation from a loss into a redirection.
