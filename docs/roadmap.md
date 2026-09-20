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

## Tier 0 — the bottleneck

### Tool registration

No spec. The decision is unmade, and it is the one that shapes most of what
follows.

Tools live inside the guest component: declared in one list and dispatched by
matching on the name, so adding one means editing `agents/default/src/lib.rs`
and rebuilding the committed wasm. Nothing above the sandbox boundary can add a
tool to a turn. An operator installing an interface has nowhere to install it
to, a generated client has nowhere to be generated into, and a skill that
documents an API call has no way to make one.

What it must do: let something outside the guest contribute a tool to a turn --
its name, its description, its argument schema -- and have a call to it reach
whatever implements it. The load-bearing question is where that implementation
runs. In the guest, the definition has to cross the WIT boundary, which has no
shape for it today and means an interface change. In the host, the tool is not
something the component reasons about at all, only something the host offers
and answers on its behalf, and the guest's own dispatch stops being the whole
story. Both are defensible; they are different platforms afterwards.

Settle that before building anything that generates tools. A generator with no
seam produces code somebody pastes into the guest, which is worse than writing
it by hand.

## Tier 1 — independent

Four things that need nothing above them.

### Platform-level egress list

[egress.md](egress.md) — "An operator allowlist, by name".

Designed down to the environment variable. An operator names internal hosts the
gateway may reach, checked before the private-address refusal, empty by
default, never workspace-settable.

### Idempotency

[idempotency.md](idempotency.md).

Standalone, and live rather than hypothetical now there is a stop button: a
tool call has three outcomes rather than two, and the third -- sent but never
observed -- is what cancelling must avoid creating.

### Triggers

No spec. Needs no interface change, which is what makes it unusually cheap for
what it enables.

Today a turn begins because a person sent a message. Everything else a
workspace might want an agent to react to -- a webhook from the operator's own
systems, an inbound email, a schedule -- has no way in. The agent side does not
change: a turn still runs a conversation and returns a reply. What is missing
is a way for something other than a person to start one, with the authority to
do so and an account of who or what did.

Worth noting it is the only item here that makes agents useful to a workspace
that is not sitting in the UI, which is most of them most of the time.

### Confirm what inhibitors actually does

[inhibitors.md](inhibitors.md).

Not a build. AGENTS.md describes this as designed and unbuilt, and it is
substantially built: the strength and verdict model, `decide`, a Postgres
store, two migrations, `/v1/inhibitors`, and enforcement in both the worker and
the gateway. `Suspended` is modelled and ordered throughout.

What is unverified is whether a suspended turn parks and resumes, or is merely
outranked. That is the half human-in-the-loop needs, so the answer decides
whether the item below is a build or a wiring-up. Cheap to establish, and it
corrects a file people are meant to trust.

## Tier 2 — needs something in tier 1

### Workspace-defined egress

[integrations.md](integrations.md) — tier 3, and
[egress.md](egress.md) for the check itself.

Needs the platform list first, which is what a workspace's list is checked
against. Note this one inverts: `egress_rules` is already per-workspace and
checked first, so the work is adding the restriction and flipping the default,
not adding the capability.

### Human in the loop

[inhibitors.md](inhibitors.md) — the same mechanism, once suspension and resume
work.

Needs the confirmation above. A turn waiting on an approval is an inhibitor
contributing `suspended`; whether that already parks a turn is exactly what is
unknown.

## Tier 3 — needs the seam

### Integrations

[integrations.md](integrations.md).

Needs tool registration, the platform egress list, and workspace-defined
egress. Also needs two things that document describes and nothing implements: a
credential store that holds one credential per workspace with three possible
lifecycles, and per-agent scoping, which is a schema change plus a resolution
rule that must narrow rather than widen.

### Egress rules in the UI

No spec of its own; the rules are [egress.md](egress.md) and
[integrations.md](integrations.md).

`/v1/egress-rules` exists and has no UI, so allowing a host means an API call
today. Three levels are wanted. Platform and workspace both have a concept
behind them already -- the operator's list and `egress_rules` -- so those tabs
wait only on tier 1 and tier 2. The agent level does not exist at all:
`egress_rules` has `workspace_id` and no agent column, so that tab is a schema
change, and the rule it implements is narrowing-only.

## Tier 4

### Skill evaluation

[skill-evaluation.md](skill-evaluation.md).

Needs tool registration and integrations. The design's own example is a skill
that documents its call the way an API's documentation does, which a capable
model reads correctly and a weaker one calls as a tool name -- and establishing
that requires a skill that can actually perform something, which requires a
tool to perform it with.

The fixture this wants is a mock service the operator has allowed, a skill
declaring that host, and an agent completing a task against it. Every part of
that exists except the tool.

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
