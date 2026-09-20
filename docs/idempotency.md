# Idempotency

What happens to a write when nobody sees the answer.

## The problem

A turn is a loop: the model generates, asks for a tool, the tool runs, the
result goes back, it generates again. Tools are the only part of that loop
which touches anything outside the transcript — `fetch` reaches a workspace's
allowed hosts, `write-object` puts bytes in a bucket.

So there is a window, between a request leaving the pod and its response
arriving, where the world has changed and we do not know it. Anything that ends
a turn inside that window — a crash, a lease expiring, a pod being evicted —
leaves a write whose outcome was never observed.

Not a stop: that is read at a round boundary and never lands inside the window
at all, for the reasons under *Stop solved this differently* below. What is
left is the set of endings nobody chose.

Today nothing records that. The turn fails, the job is retried, and the retry
runs the tool again because there is nothing to tell it the first attempt got
as far as sending. For `fetch_url` against a search engine this is harmless.
For a `POST` that moves money it is not, and the tools that move money are the
ones a platform like this exists to run.

## What a shell gets for free, and we do not

The obvious model is Claude Code: interrupt a command, and the shell sends a
signal and detaches. Whatever the command does next is its own business.

That works because a shell command is a **separate process**. It has its own
memory, its own lifetime, its own fate, and the kernel enforces the boundary.
Detaching costs nothing because nothing is shared.

A tool call here runs *inside the store*, on the guest's linear memory, on the
pod's heap, charged against the pod's admission. There is nowhere to detach it
to. Keeping an interrupted call alive means keeping the whole instance alive —
and admission charges `ASSUMED_TURN_BYTES` -- 100MB -- for a turn's lifetime,
so an interrupted turn that lingers is one that still costs a pod most of that.

There is also no signal. Nothing in `wit/agent.wit` lets the host ask a guest to
wind up; the only interruption primitives are blunt ones — drop the future,
which unwinds at an await point, or exhaust fuel, which traps. Neither is a
request the guest can answer gracefully.

So the guest is always torn down. That is not a judgement call, it follows from
where the memory lives. The judgement call is what we *record* about the call it
was suspended inside.

## Three states, not two

Most caches model two states: we have a result, or we do not. That is precisely
what breaks under cancellation, because an interrupted write silently becomes
"we do not" and gets re-executed.

The missing state is the one in the middle:

- **`completed`** — sent, answered, result recorded. Replay is exact and free.
- **`attempted`** — sent, outcome never observed. The world may or may not have
  changed.
- **absent** — never tried.

`attempted` rather than `unknown`: the transcript records events, not our
feelings about them. We know exactly what we did (sent it) and exactly what we
did not (observe the outcome). Nothing is unknown except the remote's state,
which was never ours to know.

**An `attempted` record never resolves itself.** Not on retry, not on a timer,
not by a heuristic about how likely the request was to have landed. It is
durable, and it is resolved by evidence or by a person.

## Key and cache are different mechanisms

They solve different halves and conflating them is how these systems go wrong.

**The key is for the recipient.** It is a claim about identity that a remote
system honours: send the same key twice, get the original response back, and
correctness holds even if our pod died between send and receive. This is real
idempotency, and it is the only thing that closes the window.

**The cache is for us.** It is a memory of what we did, and it can only ever be
best-effort, because the gap where our record is wrong is exactly the gap the
key exists to cover. A cancelled call cannot be cached as a *result*, only as an
*attempt*.

Which settles what to do with an `attempted` record on replay:

- **With a key** → resend it. That is what the key is for. The recipient
  dedupes, the ambiguity collapses, `attempted` becomes `completed`.
- **Without a key** → we cannot resolve it and must not pretend. It stays
  `attempted` and the model is told so.

## Three levels, opt-in

A tool declares which it wants. Default is the current behavior, so nothing
changes for tools that do not care.

1. **`none`** — nothing recorded, replay re-executes. Right for reads and for
   anything whose repetition is harmless.
2. **`cached`** — the host derives a key, records the outcome, and serves a
   replay from the record. Buys replay-within-workflow and honest `attempted`
   states. Works against recipients that know nothing about idempotency.
3. **`keyed`** — the tool accepts an idempotency key; the host mints a stable
   one and passes it through. Now `attempted` is resolvable, and the guarantee
   is end-to-end rather than best-effort.

**A properly keyed tool should never need a human.** But `keyed` is the
minority case, and building as though it were the default gets the priorities
backwards.

Stripe and the payment processors support idempotency keys because they had to.
A customer's booking system, a framework-generated CRUD endpoint, the internal
API a workspace actually points an agent at -- most of them have never heard of
one. Those recipients are the reason this document exists, not the degraded
path it falls back to.

That is the honest position: from our side this can only ever be best-effort,
and best-effort is better than the alternative, which today is a retry that
re-runs the tool with nothing recording that the first attempt sent. Even with
nothing else built, telling the model "sent, outcome not observed" removes the
silent duplicate.

## Two things to try before escalating

`attempted` with no key is unresolvable *by us*. It is not always unresolvable
by the recipient, and two declarations turn a large class of unkeyed APIs into
resolvable ones.

**A read-back probe.** Many APIs that cannot dedupe a write can still answer
whether the thing exists -- `GET /bookings?reference=...`. A tool that declares
how to ask lets the host resolve `attempted` by asking, with no person
involved and nothing required of the recipient beyond an ordinary read.

This is worth more than it first looks. It does not improve our record, which
was never the weak part: we know exactly that we sent. It asks the place that
knows the part we cannot see, which is the only way that question gets a real
answer.

The probe has to be safe to run repeatedly and must distinguish "not there"
from "cannot tell" -- a probe that errors is not evidence of absence, and
treating it as such would resolve `attempted` to "never happened" on the
strength of a network blip. Failing to confirm means still `attempted`, the
same rule the egress commitment follows: could not verify means refused.

**A natural key the recipient merely stores.** If the workspace's API accepts
a reference field and keeps it -- an invoice number, a booking reference the
agent generates -- then the write carries identity even though the API knows
nothing about idempotency. A duplicate is then detectable by the probe above,
and sometimes refused by the recipient's own uniqueness constraint, which is
the best outcome available without their cooperation.

Both are properties of the recipient rather than of the tool's own logic, which
is why they belong beside `none`/`cached`/`keyed` as things a tool declares
about what it is talking to.

Escalation remains for what neither covers: a recipient that cannot be asked
and stores nothing that identifies the write. If that fires often it is a
signal about the integration rather than about the platform.

## Key derivation is the whole ballgame

If the key is a hash of the arguments, then two legitimately distinct calls with
identical arguments — "append a line to the log", deliberately, twice — collapse
into one, and the second silently returns the first's result. That is a
correctness bug that presents as a cache hit, which is the worst kind to find.

So scope is explicit and the tool chooses, because only the tool's author knows
whether repetition is meaningful:

- **`(session, turn, call-ordinal)`** — safe default. Dedupes crash-replay of
  the same call and nothing else. Two deliberate identical calls stay distinct.
- **`(session, arguments)`** — dedupes across a session. Changes tool semantics:
  the second identical call is *defined* to be the same call.
- **explicit** — the tool names its own key from its own arguments, e.g. an
  invoice id. Best when the domain has a natural identity.

## Why the host owns it

Same argument as egress. If the guest owns idempotency, a workspace's untrusted
code decides whether its own writes dedupe, and "did this happen twice" depends
on code we did not write.

The host mints the key, the host owns the record, the host writes the three
states. The guest asks for a tool and is told what happened, which is the
existing contract — `chat` already says cancellation is "the host dropping the
stream rather than something the guest must handle", and this is that principle
applied to writes.

## Stop solved this differently, and better

This section used to describe what a stop would do with an in-flight tool call:
tear the guest down, record the call as `attempted`, synthesise a tool result
saying the outcome was not observed. The stop that shipped does none of that,
because it never stops inside a round.

`limits.cancelled` is read at the round boundary, before `chat` is called, so a
turn that is asked to stop returns what it has written and runs nothing. And a
round that the gateway cut partway is refused wholesale: the guest sees a
completion with tool calls and no finish reason, treats the arguments as
possibly truncated mid-JSON, and answers every call with "not run" rather than
executing any of them (`agents/default/src/lib.rs`).

So a deliberate stop cannot strand a tool call, and this machinery is not what
makes stop safe. Avoiding the window entirely beat recording it.

## What is still open

Everything that ends a turn *without* reaching a round boundary. A pod
evicted, a worker crashed, a lease expired while a `fetch` was in flight: the
guest is gone, the job is retried, and the retry runs the tool again with
nothing recording that the first attempt got as far as sending.

That is the case this document is for, and it is narrower than it was written
to be -- not "what a stop has to avoid creating" but what a crash creates
whatever anyone intends. It is also the case that cannot be designed away by
choosing a better boundary, because nothing chose it.

Side effects may have landed and nobody can say. That is a property of a crash
rather than of a decision, and it is the same guarantee a killed shell command
gives, so it is not a weaker position than the familiar one.

## Not yet built

Nothing here exists. Written before the tools that need it, while the design is
still free: today there is `fetch` and object storage, so the pressure is low.

The pieces, roughly in order:

- A `tool_invocations` table holding the tristate, keyed and scoped as above.
  Partitioned by workspace like everything else that grows per-workspace.
- Derivation policy in the WIT, so a tool declares its level and scope.
- The replay path in the host's tool dispatch, which is where a retry after a
  crash finds the record the first attempt left.
- The read-back probe, which is what keeps escalation rare against the
  recipients this is mostly for. Later than the tristate and before any UI for
  resolving one by hand.

Stop is no longer on that list, for the reason given above.

The pressure is still low, and it is worth being honest about why. The tools
that exist are `fetch_url` and object storage, and a repeated `write_object` is
idempotent by construction -- it replaces. So the case that needs this is a
`POST` through `fetch_url` to a workspace's own API, which is exactly what
[integrations.md](integrations.md) is about. This becomes urgent when
integrations do, and not before.

## Human in the loop

An `attempted` write that neither a key nor a probe can settle is not something
an agent should resolve by guessing, and not something the platform should
resolve by policy. "Did the payment go through, retry or not" has no correct
default. It is a person's call.

A person is the last resort rather than the first, which is the point of the
two declarations above: the goal is that escalation is rare, not that it is
well-designed.

There is no HITL yet, and this does not wait for it. An `attempted` record is
durable and resolves whenever something resolves it: a person reading the
transcript today, a HITL flow later, or an automated reconciler for recipients
that can be asked "did you get this". The record does not care which, so HITL
arrives as a UI over state that already exists rather than as a migration.

Until then the requirement is only that `attempted` does **not quietly resolve
itself** — the model is told plainly that the outcome was not observed and must
not assume either way, retry is available but never automatic, and the record is
visible in the transcript. That is a human in the loop, just synchronous and
unstructured.
