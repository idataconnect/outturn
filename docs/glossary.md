# Glossary

Words this codebase uses in a particular way, and the ones it deliberately does
not. Where a word has a document of its own, this says the sentence and links
to it.

Here because the vocabulary is load-bearing and some of it is easy to misread.
"Bundle" is the one that has already cost people time, and it has **changed
meaning**: it used to mean a single skill shipped as a body plus its files, and
the glossary warned against reading it as a set of related skills. Readers kept
making that mistake because a set is the thing they wanted. The one-skill
concept is now a **package**; a **bundle** is several skills shipped together.

## People and tenancy

**Operator** — whoever runs this deployment. Holds platform roles, ships skills
every workspace can bind, and decides which cluster-internal hosts may be
reached. Not a tenant.

**Workspace** — a tenant. The isolation boundary, and it stays that way:
organizations are added *above* it rather than nesting beneath.
[workspaces.md](workspaces.md).

**Workspace member** — an employee of a tenant, holding workspace roles. The
only kind of person with a seat besides the operator.

**A tenant's own customer** — has *no* seat, and no interface. An agent works
for a workspace member, on the tenant's systems; the customer is data the
conversation is about. A demo where a customer appears to be typing is
describing a person this platform has nowhere to put.

**Account label** — free text a workspace sets on a session, meaning whatever
its business means by it: a shipper, a customer number, a matter. The platform
never interprets it; the ledger copies it onto every row so a bill can be joined
to the workspace's own records. [usage.md](usage.md).

## Authority

**Authority** — one fixed, code-defined permission (`sessions:read`). The
vocabulary is in `rbac.rs` and changes with the code.

**Role** — a workspace-owned row bundling authorities under a name. A token
carries role *names*; the API resolves them per request through a cache
invalidated over LISTEN/NOTIFY. [authorities.md](authorities.md).

**Narrowing** — confining a person to named agents, which applies to six
authorities (`Sessions*`, `StorageAgent*`) and nothing else. `Reach` is the
per-workspace answer; `Visible` is that answer as a query filter.

## Skills

**Skill** — instructions an agent is given beside its system prompt. A row,
not a file: it has a slug, versions, and possibly a base it overrides or a skill
it was forked from.

**Body** — the part composed into the system prompt on every round of every
turn, so everything in it is paid for continuously.

**Package** — **one skill** published as a body plus a set of files, versioned
together. The body is a manifest naming operations; each file says how to call
one, read on demand with `read_object`. Built.
[skill-packages.md](skill-packages.md).

**Bundle** — **several skills** shipped, enabled and versioned as one thing,
with the hosts they reach and the credentials those need: "Accounts Receivable"
rather than any one skill in it. Designed and unbuilt,
[skill-bundles.md](skill-bundles.md). A bundle is not itself a skill and
composes into no prompt — installing one gives an agent its skills
individually, as rows in `agent_skills`, exactly as a hand-built assembly
would.

Until recently these two words meant the opposite of what they mean now. A
document or comment that says "bundle" for one skill and its files predates the
swap and means *package*.

**Override** — a skill in its own right that speaks about another's body rather
than replacing it. **Fork** — a copy that stops following its origin.

**Frontmatter** — YAML between `---` fences at the top of a package's file, for
the *platform* rather than the model. Today one key: `approval`
([approvals.md](approvals.md)). Anything the model needs to know belongs in the
body where it can see it.

**Derived skill** — a skill generated from a stored specification and its
**annotations**, regenerated rather than edited. Designed, unbuilt.
[openapi-wizard.md](openapi-wizard.md#a-derivation-not-an-output).

**Annotation** — something a person added to one operation of a derived skill
-- a note, a preference, a hidden flag, an approval rule, an observed example --
keyed by `operationId` or method and path, so it survives the specification
changing. Never read by a turn; rendered into the version a turn reads.

## Work

**Turn** — one pass of the agent loop answering one prompt, run by a runtime,
recorded as a job. **Round** — one model call inside a turn; a tool loop is
several rounds. Stopping happens at a round boundary, never inside one.

**Job** — a queued unit of work. States: `pending`, `running`, `succeeded`,
`failed`, `cancelled`, `parked`. Enumerated in more places than the schema —
grep for the strings, not just the constraint.

**Parked** — a turn a suspended hold refused, kept rather than completed, and
given back to the queue when the hold lifts. Holds no lease, so the reaper
leaves it; not counted by `job_backlog`, because no pod can claim it.

**Serial key** — work that must not run beside itself. One job per key runs at a
time, so turns in one conversation are answered in order.

**Lease** — what joins a claim to the runtime running it. Renewed while results
arrive, reaped when they stop.

## Holding and asking

**Inhibitor** — a hold on work. Zero or more apply; the strongest wins and the
verdict is derived rather than stored. Scoped platform, workspace, agent or
session. [inhibitors.md](inhibitors.md).

**Stopped** / **Suspended** — the two strengths. A stop ends the turn and
latches the session until a person says something; a suspension parks the turn
and resumes of its own accord when the hold lifts.

**Latch** — `agent_sessions.stopped_at` and `stopped_reason`, cleared only by a
prompt from a real person whose turn then runs. A suspension takes none.

**Action item** — something waiting for a person, with its targets beside it.
The queue is a read model; the hold is the truth of whether it is still open.
[action-queue.md](action-queue.md).

**Target** — who an action item waits on: a role or a user. Stored as the role,
never expanded to its members, so membership changes need no queue writes.

**Kind** — what an action item is, as `family.act`. An approval is
`approval.<act>`, built by the server from the `requires` of its frontmatter, so
`requires: charge` becomes `approval.charge`. Not `hitl.*`: that spelling was in
the component and the fixtures while the only producer emitted `approval.*`, so
every real row rendered with the fallback label and the wrong glyph, and the
tests agreed with each other rather than with the code.

**Approval** — a person's yes to an act an agent is about to take. Declared in
frontmatter, answered from the queue, and worth a capability the retry carries.
[approvals.md](approvals.md).

**Capability** — what an approval mints: a grant, `approval_grants`, recording
what was approved and how far it reaches -- this call and its retries by default,
or one unit for the rest of the turn when the approver ticked `covers` -- so a
resumed turn does not ask again. Keyed on the gate's bound fields, so a
different charge is a fresh question. Travels in the turn token, and the gateway
reads it there. [approvals.md](approvals.md#what-a-yes-is-worth).

**Auto-approval policy** — a workspace's standing answer to a gate: for an
agent, a selector (risk, act or operation) and conditions on the gate's bound
fields. Decided at the gateway from the turn token, recorded there before the
request goes out, and set only by somebody who could have answered it.
Designed, unbuilt. [auto-approval.md](auto-approval.md).

**Proposal** — what the admin agent writes instead of a change. Confirmed by a
person through an endpoint the agent's token cannot reach. Designed, unbuilt.
[admin-agent.md](admin-agent.md).

**Gate** — a request shape a turn may not send without somebody's word: a host,
a method and a path, declared in an operation's frontmatter. The opposite of an
egress rule in the direction it fails. A rule is a permission, so failing to
prove one means refused; a gate is an obligation, so absence read as "nothing is
gated" would let everything through. Hence a commitment of its own, an empty set
that is signed rather than absent, and a token with no gate claim refused
outright.

## Reaching out

**Egress rule** — a host a workspace's agents may reach, with the *name* of the
environment variable holding its credential. Never the value.

**Sealed credential** — a secret encrypted in the browser to the gateway's
public key, with its binding (workspace, hosts, header) as associated data, so
the database holds it, the API can read where it goes, and nobody but the
gateway can use it or change that. Built for static headers; client-credentials
pairs still name variables. [sealed-credentials.md](sealed-credentials.md).

**Commitment** — the API's signed hash over a turn's egress rules, carried in
the turn token. A request offers a rule and a proof; the gateway checks it
against the root inside the token, so a runtime that rewrote its own copy gets
nowhere. Could not verify means refused. `src/egress/commit.rs`.

**Operator allowlist** — `OUTTURN_INTERNAL_HOSTS`. Cluster-internal addresses
are refused by default; this is how somebody outside the workspace names the
exceptions.

## Events and the feed

**Event** — something that happened, appended to `events` and read from a
cursor. Stays true for ever; its per-user state is read or unread.

**Cursor** — the last event id seen. UUIDv7 keys carry ordering, so there are no
sequence columns and no offset pagination; `Uuid::nil()` means the beginning.

**Watermark** — how far a read looked, visible or not, so a narrowed reader's
cursor can advance over rows they may not see.

**Hint** — a payload-free notification that something was appended. Listeners
re-query from their own cursor, so a coalesced or dropped hint costs a poll
rather than an update.
