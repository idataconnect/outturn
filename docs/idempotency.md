# Idempotency

What happens to a write when nobody sees the answer.

## The problem

A turn is a loop: the model generates, asks for a tool, the tool runs, the
result goes back, it generates again. Tools are the only part of that loop
which touches anything outside the transcript — `fetch` reaches a workspace's
allowed hosts, `write-object` puts bytes in a bucket.

So there is a window, between a request leaving the pod and its response
arriving, where the world has changed and we do not know it. Anything that ends
a turn inside that window — a stop, a crash, a lease expiring, a pod being
evicted — leaves a write whose outcome was never observed.

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
and the assumed cost of a turn is 384MB, so a stopped turn that lingers is a
stopped turn that still costs a pod.

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

A tool declares which it wants. Default is the current behaviour, so nothing
changes for tools that do not care.

1. **`none`** — nothing recorded, replay re-executes. Right for reads and for
   anything whose repetition is harmless.
2. **`cached`** — the host derives a key, records the outcome, and serves a
   replay from the record. Buys replay-within-workflow and honest `attempted`
   states. Works against recipients that know nothing about idempotency.
3. **`keyed`** — the tool accepts an idempotency key; the host mints a stable
   one and passes it through. Now `attempted` is resolvable, and the guarantee
   is end-to-end rather than best-effort.

**A properly keyed tool should never need a human.** Escalation is the fallback
for recipients that do not support keys. If it fires often, that is a signal the
tool integrations are weak, not that the platform is working as designed.

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

## What this gives stop

A stop is then a read of this machinery rather than a special case:

- Drop the guest future, the store, and any in-flight host call. Return the
  permit. Stop is immediate and holds no memory.
- Record the in-flight tool call as `attempted`.
- Synthesise a tool result saying so, because the conversation format requires
  every `tool-call` to be answered and a turn truncated between them replays a
  call with no result. It says the call was started and its outcome not
  observed — not that it failed, and not that it did not happen.
- Keep the partial reply, marked stopped.

Dropping an in-flight `fetch` rather than letting it complete is deliberate:
egress is host-enforced and authorised per turn, so a finished turn should not
still be spending its authority.

Side effects may have landed. That is a property of stop, stated rather than
papered over — and it is the same guarantee a killed shell command gives, so it
is not a weaker position than the familiar one.

## Not yet built

Nothing here exists. Written before the tools that need it, while the design is
still free: today there is `fetch` and object storage, so the pressure is low.

The pieces, roughly in order:

- A `tool_invocations` table holding the tristate, keyed and scoped as above.
  Partitioned by workspace like everything else that grows per-workspace.
- Derivation policy in the WIT, so a tool declares its level and scope.
- The replay path in the host's tool dispatch.
- Then stop, which reads it.

## Human in the loop

An `attempted` write with no key is not something an agent should resolve by
guessing, and not something the platform should resolve by policy. "Did the
payment go through, retry or not" has no correct default. It is a person's
call.

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
