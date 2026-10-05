# The action queue

What is waiting for a person to do something about it, and how they find out.

## Two things, not one

A notification system for this platform is two mechanisms that look like one,
and building them as one makes each worse.

**The event feed** is what happened. It is the `events` table, read from a
cursor, and an entry stays true for ever: a turn that completed at 14:32
completed at 14:32 whoever reads it later. Its per-user state is *read or
unread* -- a fact about the reader, not about the event.

**The action queue** is what is waiting on somebody. Its truth condition is the
opposite: an item is true until it is settled, and it leaves the queue when a
colleague answers it rather than when the reader has seen it. Its state is a
lifecycle, not a reading.

Folding one into the other means giving the log a state machine or giving the
queue a cursor, and neither is an improvement on two tables. It also makes the
per-user storage wrong in whichever direction you fold: read/unread on a queue
nags about decisions somebody else already made, and a lifecycle on a log
rewrites history.

The event feed half is scoped in [roadmap.md](roadmap.md), under
*Notifications, and the inbox that shows them* -- the second poll loop, read
state as a cursor or a row per item, and which kinds reach the inbox. Nothing
here changes that. This document is the queue.

## The queue is a read model. The hold is the truth

[inhibitors.md](inhibitors.md) already says what a pending approval *is*:

> HITL is an inhibitor of strength `suspended`, held by a pending request and
> released when somebody answers it. It is not a tool result.

So a pending human-in-the-loop request is an `inhibitors` row. It is what gates
the work, `worker::inhibited` already consults it at every checkpoint, and
`release` is already how it stops being pending.

The queue does not restate that. Storing a `pending`/`resolved` column beside a
hold that is itself either held or released puts one fact in two places, and
[invariants.md](invariants.md) opens with what that costs here: `cancelled` was
added to a check constraint and two SQL lists and missed in two others, and a
session wedged permanently. A queue row saying "still waiting" for a hold
somebody released is the same bug, and its symptom is a person answering a
question that was already answered.

What the queue adds is everything the hold cannot express:

- **Who should answer.** `inhibitors` is scoped platform, workspace, agent or
  session. There is no room in it for "the finance role", and the doc leaves
  *may this person answer approvals at all* as an authority that does not exist
  yet.
- **One list across workspaces.** A hold belongs to one workspace. A person
  belongs to several, and the workspace they are not looking at is exactly where
  an unseen decision sits.
- **A badge.** One number for the whole application, which no per-workspace read
  can give.

So: the hold decides whether the work runs and whether the request is still
open. The queue decides who is asked and renders the asking.

### Which way round, concretely

An item names its hold rather than carrying its state:

- `inhibitor_id` on the queue row, not an `event_id` and not a state column.
- Open means the hold is still held. Derived, the same way
  `inhibitor::decide` derives a verdict rather than storing one.
- Answering is `release(id)` through `InhibitorStore`, which is where the race
  belongs: two people answering at once should contend over the thing that gates
  the turn, not over a row that gates nothing.
- Expiry is the hold's business too. A queue that expired its own row while the
  hold stayed held would hide a suspended turn rather than resolve it.

The cost is a join to read the queue, and a queue row outliving its hold when
something deletes one without the other. Both are cheaper than two copies of
"is this still waiting".

## What is built

The targeting and the reads, on `action_items` and `action_targets`, behind
`ActionStore`. Raising, adding and removing targets, the per-workspace reads,
the global read and badge, a Postgres listener with a long poll over it, and
`GET /v1/action-items` with its `count`.

Two decisions in there are worth keeping whichever way the rest goes.

**A target is a role, stored as the role.** Expanding it to its members when the
item is raised freezes a snapshot of membership into a row meant to outlive the
moment: somebody joining tomorrow never sees today's request, somebody leaving
keeps it, and a request raised while the role is empty resolves to nobody and is
silently lost. Membership is joined when the queue is read instead, so a role
changing hands writes no queue rows at all.

**The global read takes no workspace argument.** A token carries one workspace
and the answer spans every workspace the reader belongs to, so there is nothing
to resolve an authority against -- and gating on the token's workspace would be
worse than nothing, since a role grant in one workspace would license reading
another's. The workspace set is derived from the reader's own role grants and
there is deliberately no parameter for it. Both reads return only items
addressed to the caller, so there is nothing to escalate to.

That boundary is the one the schema does not hold. `workspace_id` is in every
key, so a query that drops it fails loudly; the line between two people *inside*
one workspace is a role comparison in one `exists` clause, and a mistake there
is quiet. It is checked by mutation: with the role match dropped, three tests
fail naming the leak, and none of the cross-workspace tests notices.

### What is built wrongly, by the reasoning above

`action_items.state`, `resolved_by`, `resolved_at`, `expires_at`, `settle()` and
`sweep_expired()` all model a lifecycle the hold should own. They were written
before this document, against `event_id` rather than `inhibitor_id`, and they
work -- there are tests for the settle race and the expiry sweep -- but they are
the second copy this document argues against.

They stay until suspension exists, because deleting them now would leave the
queue unable to express "waiting" at all, and be a second rewrite when the hold
arrives. What they must not acquire meanwhile is a caller: nothing should start
depending on `state` as the answer to "is this open".

## Order of work

[inhibitors.md](inhibitors.md) gives it, and this document is late to it:

> **Notifications after that.** They look independent and are not: until
> inhibitors exist there is nothing to notify about, and what the notification
> system has to carry is decided by what generates the events.

That was right and the queue was built first anyway, which is how it came to
point at `event_id`: the payload was designed without knowing that the thing
generating these carries a `reason`, a `held_by` and a scope.

What remains, in order:

1. ~~**Suspension.**~~ Built. A suspended verdict parks the job rather than
   completing it, and releasing the hold gives it back, scoped the way the hold
   was. The serial key needed no change -- the claim excludes keys that are
   `running` -- and there is a test that fails if that ever widens to parked.
   What still cannot happen is a suspended hold being *taken*: both endpoints
   write `Strength::Stopped`.
2. **`approvals:answer`**, the authority for *may this person answer approvals
   at all*. Ordinary, and held by whoever a workspace decides. What a yes is
   worth once given, and what declares that an operation needs one, is
   [approvals.md](approvals.md).
3. **The producer.** Something that takes a suspended hold and raises a queue
   item naming it. Only now is it clear what that item carries.
4. **Answering**, which releases the hold and lets the parked turn resume.
5. **Repointing the queue** at `inhibitor_id`, and deleting the state columns
   above.

The capability an approval mints, and what its extent is, stays where
[inhibitors.md](inhibitors.md) leaves it: open, and not blocking any of the
above.

## Web push, later

A browser that is asleep cannot be reached by any of the above: a long poll
needs a live connection, and a backgrounded tab is throttled to nothing.

Web Push is the mechanism, and its constraints decide its shape rather than
leaving it open. Payloads are capped near 4KB, and Chrome and Firefox require
that every push shows a visible notification -- so it cannot be a silent "go
refresh", and it cannot carry the item. What it can be is a nudge naming the
workspace, deep-linking to the queue, which the client then reads properly.

Three things follow, and none needs deciding before the work above:

- **A subscription is per browser, not per workspace.** One device serves every
  workspace its owner belongs to, which makes it the first thing here that is
  not workspace-partitioned.
- **Coalesce on the `Topic` header.** A later push with the same topic replaces
  an undelivered earlier one, so a phone asleep through a busy hour wakes to one
  notification per workspace rather than a pile. Lossless, because the payload
  is a link.
- **Prune on 404 and 410.** Endpoints rot constantly -- reinstalls, profile
  resets, revocations -- and an unpruned table pays outbound requests on dead
  endpoints for ever.

Do not push to somebody who is connected and looking at the queue. The badge has
already moved, and a phone buzzing for something they watched arrive is the
fastest way to lose the permission.
