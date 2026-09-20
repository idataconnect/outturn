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

No spec. Reads a specification and produces what a workspace needs to integrate
against it -- today that means skill text: the endpoints, their arguments, what
comes back, written the way a model reads well rather than the way an API
reference is organised.

Worth building early precisely because it needs nothing. It is also the thing
that makes the question below answerable with evidence: a wizard that generates
prose will show where prose is not enough, in which case the same wizard
generates tool definitions instead and only the consumer changes.

### Platform-level egress list

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

## Tier 3 — needs tiers 1 and 2

### Integrations

[integrations.md](integrations.md).

Needs the platform egress list and workspace-defined egress. Also needs two
things that document describes and nothing implements: a credential store that
holds one credential per workspace with three possible lifecycles, and
per-agent scoping, which is a schema change plus a resolution rule that must
narrow rather than widen.

Not tool registration. An integration is a permitted host, a bound credential,
and a skill saying what to call -- all of which `fetch_url` already serves.

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
