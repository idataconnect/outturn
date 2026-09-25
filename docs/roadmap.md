# Roadmap

What is next, in an order that respects what depends on what.

Ordered by dependency rather than by value. Everything in one tier can be
started without waiting for anything else in it; a tier's items need something
above them first. Where a design already exists it is linked rather than
restated, and the link is the specification -- this file records order, not
substance.

Nothing here is a commitment to build in this order, or at all. It is what the
dependency graph permits, so that choosing differently is a decision rather
than an accident.

## Tier 1 — independent

Five things that need nothing above them.

An earlier draft of this file put tool registration above them all, on the
grounds that integrations could not work until something outside the guest
could contribute a tool. That was wrong, and the correction is worth recording
because it is the kind of mistake that builds a platform nobody needed.

`fetch_url` already reaches any host a workspace is allowed, so an integration
is a skill documenting some endpoints plus a rule permitting the host. More to
the point, a typed tool would offer no containment a skill does not: the
boundary is the egress rule and the credential the gateway attaches, and a
generated `create_booking` would be checked by exactly the same code as a
`fetch_url` call to the same place. What a typed tool buys is argument *shape*
-- a weaker model cannot malform a request it did not compose -- and argument
*constraints*, which is where a conduit's sender domain or recipient list would
be pinned. Both are real and neither is a prerequisite. See
[the fork below](#a-fork-not-a-prerequisite).

### OpenAPI integration wizard

[openapi-wizard.md](openapi-wizard.md).

Reads a specification and produces a skill, split the way the guest's own tools
already split: a manifest small enough to carry on every turn, and a file per
operation in `workspace/` scope that the agent reads with `read_object` when it
needs one. That is what lets it include every operation without the prompt
growing with the API.

Worth building early because it needs nothing that does not exist -- the object
store, `read_object`, `skill_version_hosts` and the files API are all in place.
It is also what makes the fork below answerable with evidence rather than
argument.

And the premise now has evidence of its own. One run was written out by hand
for Hollowbrook on 2026-09-22 -- `k8s/components/hollowbrook/skill/` is what a
wizard run would produce -- and the agent read the detail file before calling,
every time, including after the pattern was obvious. The whole design fails if
a model guesses the call from a manifest line, so that is the assumption worth
having tested before building on it. See *The shape, tried once* in
[openapi-wizard.md](openapi-wizard.md).

### Skills as bundles

[skill-bundles.md](skill-bundles.md).

A skill version as a body plus a set of files, versioned together and read
through a `skill/` scope resolved against the version the turn bound. Today the
detail sits in `workspace/` scope beside it: overwritten in place while the
body is versioned, owned by nothing, editable by anyone with workspace storage
write, and unreachable for an operator's skill. Worth doing before the wizard,
which should write into this shape rather than fix the current one in place.

### Prompt caching

[caching.md](caching.md).

Anthropic caches only what a request marks, and nothing is marked, so every
turn on it is uncached input. Four breakpoints, each a cascade setting: the
instructions, the compacted history and the last completed turn on by default,
the current tool loop off. Compaction has to cut in batches for any of it to
hold, so that belongs in the same change.

### Platform-level egress list — an escape hatch, not a priority

[egress.md](egress.md) — "An operator allowlist, by name".

Designed down to the environment variable, and deliberately configuration
rather than a table or a UI: something changeable only by redeploying is the
strongest form of operator-only, since there is no endpoint to guard.

Downgraded on 2026-09-20 after asking who actually needs it. A customer's
service usually has a public name, and reaching `tickets.acme.com` needs none
of this -- ordinary rule, public address, credential over https. The list is
for a service with no public name at all, sitting beside outturn in the
cluster. Real, and narrower than the design first claimed.

It is also the path that permits a credential over plain http, so it is the
less safe of the two and should not be the recommended one.

Nothing today needs it, so the first real operator who does is better evidence
than anything decided now.

### Idempotency

[idempotency.md](idempotency.md).

Standalone, and smaller than it was written to be. The design's motivating case
was the stop button, and stop solved it another way: cancellation is read at a
round boundary and a round cut partway has its tool calls refused wholesale, so
a deliberate stop cannot strand a call.

What remains is the case nobody chooses -- a pod evicted or a lease expired
while a `fetch` is in flight, where the retry re-runs the tool with nothing
recording that the first attempt sent. Real, and not designable away by picking
a better boundary.

Low pressure today: the tools that exist are `fetch_url` and object storage,
and a repeated `write_object` replaces rather than duplicates. It becomes
urgent when a workspace is POSTing to its own API, which is when integrations
do.

### Triggers

[triggers.md](triggers.md). Schedules first; webhooks and email designed and
deferred.

The only item here that makes agents useful to a workspace not sitting in the
UI, which is most of them most of the time.

Schedules are cheap because the queue was built for them: `jobs.run_after`
schedules work forward and the claim reads `priority` first, so background
turns cannot get in front of somebody waiting. Both exist and are tested.

Two things it surfaces rather than solves, both recorded in that document:
notifications, which is where a reply nobody asked for and a repeatedly failing
schedule both land, and a dashboard panel for agent health. Neither exists, and
triggers are what make their absence matter.

### ~~Confirm what inhibitors actually does~~ — done, 2026-09-20

Stopping is built and suspension is not, in the precise sense that matters:
**nothing can take a suspended hold.** Both endpoints hardcode
`Strength::Stopped` (`src/api/router.rs`), so `Strength::Suspended` appears
only in the enum, its parser, the verdict mapping and unit tests. The
`Verdict::Suspended` arm in `worker::inhibited` is reachable only by a row
written straight to the database.

What that arm does when reached is refuse the turn without latching: no
requeue, no parked state, the job completes, and the next turn re-evaluates.
So a suspension does not park and resume today -- it declines, and something
must arrive later to try again.

That makes human-in-the-loop a build rather than a wiring-up, and says what
the build is. See its entry in tier 2.

## Tier 2 — larger, and each needs a decision made first

Neither of these waits on tier 1 any more. Workspace-defined egress never did,
and human-in-the-loop was waiting on a question tier 1 has now answered. They
are here because each is substantial and each rests on something settled rather
than obvious -- not because something above has to be built first.

### Workspace-defined egress

[integrations.md](integrations.md) — tier 3, and
[egress.md](egress.md) for the check itself.

Does not need the platform list, which an earlier draft claimed. The two
answer different questions: the platform list says whether an *internal* host
is reachable at all, while this governs who may add rules for public ones. A
workspace permitted to add hosts is permitted to add public hosts, and the
address check it cannot waive is what keeps those apart.

Note this one inverts: `egress_rules` is already per-workspace and checked
first, so the work is adding the restriction and flipping the default, not
adding the capability.

### Human in the loop

[inhibitors.md](inhibitors.md) — the same mechanism, once suspension and resume
work.

A build rather than a wiring-up, now that the tier 1 item above has established
why. Three pieces, none of them the verdict model, which is done:

1. **Something that takes a suspended hold.** Both stop endpoints hardcode
   `Strength::Stopped`, so nothing can create one. Whatever asks for approval
   is what takes it, which means the shape follows from the approval flow
   rather than from the inhibitor API.
2. **A turn that parks rather than declines.** `Verdict::Suspended` currently
   refuses the turn and completes the job, so nothing is left to resume. A
   held turn has to remain claimable -- a requeue with a `run_after`, or a
   state the claim skips until the hold lifts -- and that choice interacts
   with the serial key, since a parked turn must not block its session's
   queue for ever.
3. **Resumption when the hold lifts.** A stop waits for a person by design;
   a suspension is supposed to run again of its own accord. Releasing the
   hold is the event, and something has to notice it and give the turn back
   to the queue.

The reader-facing half is already there: `chat.held` carries a `resumable`
flag, true for suspensions, and the worker already announces it.

What it does not have is a way to tell somebody who is not looking at that
session. An approval nobody hears about is a turn parked for ever, so this
wants the inbox under [Undesigned, and wanted](#undesigned-and-wanted) --
not as a prerequisite, since a workspace watching one session would manage,
but as the thing that makes it usable by anybody else.

## Tier 3 — needs tiers 1 and 2

### Integrations

[integrations.md](integrations.md).

Needs workspace-defined egress, and two things that document describes and
nothing implements: a credential store that holds one credential per workspace
with three possible lifecycles, and per-agent scoping, which is a schema change
plus a resolution rule that must narrow rather than widen.

Not the platform egress list. A workspace integrating with Notion or a
customer's public API never touches the internal path.

Not tool registration. An integration is a permitted host, a bound credential,
and a skill saying what to call -- all of which `fetch_url` already serves.

### Egress rules in the UI

No spec of its own; the rules are [egress.md](egress.md) and
[integrations.md](integrations.md).

`/v1/egress-rules` exists and has no UI, so allowing a host means an API call
today. That is the gap worth closing, and it is the workspace level: the table
exists, the endpoint exists, and nothing but a page is missing.

The agent level does not exist at all -- `egress_rules` has `workspace_id` and
no agent column -- so it is a schema change plus a resolution rule that narrows
rather than widens.

The platform level should be *shown* and not edited. The internal-hosts list is
configuration on the gateway on purpose, so a page that let somebody change it
would undo the thing that makes it operator-only. Displaying what is currently
set is useful; offering a save button is not.

## Tier 4

### Skill evaluation

[skill-evaluation.md](skill-evaluation.md).

Needs integrations, so that there is a skill worth evaluating. Not tool
registration: the design's own example is a skill documenting its call the way
an API reference does, which a capable model reads correctly and a weaker one
calls as a tool name -- and that failure is *about* prose-driven calling, so it
cannot be studied on a platform where prose-driven calling was replaced by
typed tools first.

The fixture this wants is a mock service the operator has allowed, a skill
declaring that host, and an agent completing a task against it with
`fetch_url`. Every part of that exists today.

## A fork, not a prerequisite

### Tool registration

No spec, and no scheduled place in this list, which is deliberate.

Tools live inside the guest: `all_tools()` is a literal vector and `run_tool`
matches on the name, so the set is fixed when the wasm is built. Nothing
outside can add one. The question is whether that matters, and the answer
depends on evidence nobody has yet.

What a registered tool would buy is argument shape and argument constraints. A
model cannot malform a request it did not compose, which is a hedge against
weaker models rather than an architectural need; and a constraint can pin a
field the agent must not choose, which matters for a conduit integration --
a verified sender domain, a bounded recipient list -- and for little else.
Neither is containment: the egress rule and the credential binding are the
boundary, and a typed tool is checked by the same code as a `fetch_url` call to
the same host.

Where it would earn its place is a multi-step operation -- several correlated
calls threading identifiers through, where a model will sometimes get it wrong
in ways that are hard to notice. That is a real case and it has not arrived
yet.

So: build integrations on `fetch_url`, and let a specific integration that
prose cannot serve be the thing that justifies this. When one does, the
decision to settle is where a registered tool runs. In the guest, its
definition must cross the WIT boundary, which has no shape for it today and
means an interface change. In the host, it is not a tool the component reasons
about at all, only one the host offers and answers on its behalf, and the
guest's dispatch stops being the whole story. Both are defensible; they are
different platforms afterwards.

## Done, and not from this list

Work that answered something found by using the thing rather than by reading
the plan. Recorded because the list above says nothing about it, and a reader
comparing the two would otherwise think it never happened.

**Open source readiness** — 2026-09-21. CI running the tier a bare checkout
can run, `SECURITY.md` with the boundary stated rather than boilerplate,
contributing guide, code of conduct, `NOTICE`, and clippy enforced for the
first time across the tree. That last one found a real leak: the provider
stream tests wrapped the mock in the struct whose `Drop` kills it only after
the readiness wait succeeded, so a timeout left `mockllm` holding a port.

**Two bugs found by signing in as somebody else** — 2026-09-21. An operator
was seeded without `settings:read` while a viewer had it, which made operator
the only role that was not a superset of the one beneath it; operator now also
holds `usage:read`, since whoever holds `gateway:invoke` is spending the money
and the usage window is how runaway spend gets noticed. A viewer was offered a
start button that threw and a composer that took a message and dropped it. A
test now asserts the roles form a ladder.

Worth repeating before any release: the authority system had only ever been
seen from an account that bypasses every check.

**Composer and transcript work** — 2026-09-21. One mark now carries a turn
from asked-for to finished, replacing a spinner under the prompt that handed
over to a different animation under the reply. Files can be dropped on the
composer. A reply can be copied. A passage can be quoted back out of one. And
`/` lists the skills an agent actually has.

The slash menu is the one worth reading about, because it went wrong twice in
the same way. The library's default writes `:command[Name]{name=slug}`, which
nothing here renders -- so the model received markdown-directive syntax raw,
said it was not a syntax it responded to, and then invented a fact rather than
stopping. Replacing it with a sentence then wrote the slug, which reaches the
model nowhere at all: `src/api/skill/mod.rs` composes each skill into the
prompt as a `## {name}` heading and `ResolvedSkill` has no slug field. Both
were found by a person trying it, not by a test.

**Hollowbrook as a component, and one wizard run by hand** — 2026-09-22.
`scripts/dev-mac.sh --with hollowbrook` brings the guesthouse up, opens its
host, and installs a skill describing its API -- so the platform's own
demonstration is one flag.

The installation is a Job calling the public API rather than anything in the
platform. The first version was a Rust module in `src/api/` with the skill
embedded by `include_str!`, which worked and was the wrong answer: a customer
wiring up their own service cannot add a module to outturn, and a fork to do it
loses to every upgrade. What the Job does -- sign in, create a skill, approve
its host, upload its files -- is what a customer does, using endpoints that
already exist.

It also found the two halves of one decision disagreeing.
`OUTTURN_INTERNAL_HOSTS` let the gateway connect to a bare name like
`tickets`, and the rule validator refused any host without a dot -- so an
operator could open a path and nobody could write the rule that would use it.
The validator consults the same list now, and the API reads it too, being the
tier that validates.

## Undesigned, and wanted

Named by other work rather than chosen, which is the usual way a gap is found.

**Notifications, and the inbox that shows them.**
[triggers.md](triggers.md) needs them twice over: a reply nobody asked for is
useless if nothing says it arrived, and a schedule failing every morning is a
broken integration nobody is watching. Human-in-the-loop needs them a third
time, since an approval nobody is told about is a turn parked for ever.

Less undesigned than it reads. The second channel already exists end to end:
`events.session_id` is nullable, `events_workspace_id_idx` indexes the
workspace-wide read, and `/v1/events` takes `session_id` as an `Option` -- so
omitting it polls the whole workspace today, narrowed by `Visible::of` like
every other read. Nothing consumes it.

What is actually missing:

1. **A second poll loop in the browser**, keyed on the workspace rather than
   the open session, running whether or not a session is open.
2. **Read state**, which is the only genuinely new storage. Either a
   per-user high-water cursor -- cheap, and the shape the poll already
   speaks -- or an `event_reads` row per `(user_id, event_id)`, which is
   what per-item dismissal needs. The cursor cannot express "dismiss this
   one and keep that one unread", so the choice is whether that matters.
3. **The inbox itself**, and what a workspace can say about which kinds
   reach it.

Not `QueueItemPrimitive`, which renders assistant-ui's own composer queue --
the lanes this project deliberately leaves empty, because a message is
persisted the moment it is sent and a browser-held copy would be a second
source of truth that vanishes with the tab. An inbox is the opposite case:
the server is the origin, so there is nothing local to disagree with it.

**Showing what the model was thinking.** Thinking blocks are dropped at
`src/gateway/llm/translate.rs`, with a comment saying they cannot be replayed
to the provider so keeping them would only put them in a transcript that must
not send them back. That conflates two questions. What a provider will accept
back is one thing; what a person may see is another, and the second does not
follow from the first.

The real constraint is narrower: Anthropic wants a thinking block returned
with its signature when a turn continues into tool use, so the block and its
signature have to be kept together and replayed where the API expects them,
and dropped where a provider has no equivalent. That is the ordinary shape of
multi-provider mapping rather than a reason not to store them.

Four pieces: keep the block and signature instead of discarding them; store it
as a part the transcript can tell from output; replay or drop per provider at
the gateway; and a workspace setting that gates display. Off by default,
because thinking is less filtered than output and a model sometimes reasons
about things it does not say -- which is a reason for a workspace to choose it
deliberately, not a reason nobody may have it.

`ChainOfThoughtPrimitive` renders it once there is something to render, and
brings its own accordion.

**A dashboard panel for agent health.** The smallest useful version of the
above, and it reads rows `/v1/usage` already carries.

**Per-agent narrowing on trigger creation.** Creating a schedule or a webhook
is a way to make an agent run turns, and neither checks the per-agent narrowing
that `sessions::create_session` enforces with `require_for_agent`. Somebody
scoped away from an agent can still give it a trigger. Small -- one call in two
handlers -- and it is a documented rule the code does not follow, so it should
not wait for a bigger piece of work. See [triggers.md](triggers.md).

**Flagging a session as wrong.** Nothing today lets a person say a turn did the
wrong thing. [skill-evaluation.md](skill-evaluation.md) needs it twice over,
and the second is the surprising one.

As a correction channel: a flag retracts what was derived from that session,
and repeated flags against one operation say the signal is unreliable there.

And as the *only labelling that will ever happen*. Every turn stored is real
and none of it says whether it went well, so a held set of cases to test a
skill edit against has to come from somewhere -- and the alternatives are
asking a workspace to author test cases, which is a second job, or inventing
them, which cannot show that prose is confusing because whoever wrote them
already knew what it meant. A flagged session is real inputs, a real failure
and a person's verdict, for free. Enough of them is a corpus nobody wrote.

Small on its own -- a column and a button -- and load-bearing out of proportion
to that.

**Memory, or whatever the durable thing turns out to be.** A schedule running
weekly in a fresh session each time cannot learn anything, and the fix is not a
long-lived session. What it wants is narrow and durable, and the shape is
unsettled: an agent writing notes for its future self is a judgement about what
mattered, which is nearer compaction carry-over than user-declared memory, and
AGENTS.md is explicit that those must not share a store. It is also partly an
evaluation problem, since knowing what was worth keeping means knowing what
went wrong without it.

## Not on this list

Recorded so their absence is deliberate.

**Compaction carry-over.** Designed in AGENTS.md under Compaction. Independent
of everything here, and the trim beneath it already guarantees a turn never
fails for context.

**An end-customer surface.** A guest signing in to talk to an agent is a
different product with different auth, and
[integrations.md](integrations.md) explains why the deployment this platform is
for does not need one. The row-level authorization it would require is not on
this path.

**Redis caching, OpenTelemetry, per-workspace usage attribution, workflows as
scripted tasks in sub-sessions.** AGENTS.md lists these as intended. None
blocks anything above.
