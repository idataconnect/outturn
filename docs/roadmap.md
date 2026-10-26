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

## Now, in order

The working queue: what is being done next, short enough to keep in your head.
Everything below it is the longer view. An item leaves this list when it is
committed and verified, and new work goes in where it belongs, not at the end.

1. **What the review of 2026-10-20 left** (*From the review*, below): the
   ten fixes, the retention sweeps and the indexes are in; name uniqueness
   (in the order given there) goes next, then reviewing the wizard's
   suggestions (*Undesigned, and wanted*, below), then the prompt and sandbox
   boundaries and the rest in the order given there.
2. **What is left of triggers** ([triggers.md](triggers.md)): one form with a
   common half for every kind, attachments on mail, then IMAP.
3. **A first real call to Amazon Bedrock**, by whoever first has an AWS
   account: everything in [bedrock.md](bedrock.md) is tried against
   stand-ins only, and the two-doors recipe says so until then.

## From the review

A read of the whole tree on 2026-10-20, by area, with every finding checked
against the code before it was kept. The ten that could lose data, money or
a tenant's isolation went straight to branches: user routes scoped to the
caller's workspace, logout revoking the refresh family, the lease kept alive
through preparation, a job-state enum and the placeholder discarded on a
final prepare failure, one definition of prompt tokens, vision billed and
translated, schema expansion and host reads bounded, streams decoded across
chunk boundaries and provider URLs kept out of transcripts, pods hardened,
and timer firings made once-only with session delete taking its work along.

What follows is the remainder: each is the exact change, with where, so it
can be picked up without rereading the review. Ordered by what it protects.

### Indexes and constraints

- No `(workspace_id, name)` uniqueness on `models`, `provider_accounts` or
  `chains`, though the UI lists and picks by name, and none on
  `credentials.name` for either the platform's or a workspace's. Not a
  migration alone: duplicates are a state the code makes today. 0041 named
  each migrated model after the model it asks for, so one served for two
  traffic types became two of the same name; a model's name defaults to the
  catalog's, so one model at two accounts is two; and Platform → Set up
  models makes a "Local" account and chain afresh on every run, including the
  run after one that failed at the test call. Needs, in order: the create and
  rename endpoints answering 409 on a taken name, Set up reusing what an
  earlier run made, a migration renaming existing duplicates (a suffix,
  oldest keeps the name), then the unique indexes.
- Index migrations on `events`, `agent_messages` or `jobs` carry
  `-- no-transaction` and `create index concurrently`, one statement to a
  file (0061, 0063, 0065); 0024, 0026 and 0031 took SHARE locks under every
  streaming turn. A concurrent build waits out every older snapshot,
  including that of a replica blocked on sqlx's migration lock, so two API
  replicas starting together can deadlock it; it then leaves an invalid
  index and fails again on the next start until the index is dropped. Worth
  a migration step run once, before the replicas, rather than in each.

### Prompt and sandbox boundaries

- The summarizer promotes tool output to the user role
  (`crates/api/src/chat/summarize.rs`, the message building): results become
  "[result of a tool call]" user messages, and the summary is then stored as
  assistant text the rest of the session trusts. Wrap results in the same
  untrusted delimiters the platform preamble uses for turns
  (`crates/api/src/skill/mod.rs`, the preamble) and tell the summarizer so.
- Cancellation is not enforced by the host: `chat` in
  `crates/runtime/src/component.rs` never checks `cancelled`; only the
  default guest's loop does. Refuse a `chat` call once the turn is cancelled,
  so a component that ignores the flag still stops.
- Only `chat` rounds are capped. `fetch`, `list_objects` and `describe_image`
  have no per-turn count, and no workspace has a storage quota, so one turn can
  fill the bucket. A per-turn count on each, and a quota on `write_object`
  and `open_writer` read from workspace settings.
- Session files expire by object age, not session activity
  (`crates/core/src/storage/s3.rs`, the lifecycle rule). A session older than
  the TTL loses its uploads mid-conversation, and `sleep` permits exactly that
  span. Expire on last activity, and sweep `extracted/sessions/` on the same
  clock so a file the listing no longer shows is not still readable.

### The gateway

- Non-stream failover re-sends a request the provider already accepted
  (`crates/gateway/src/router.rs`, the route loop): a read timeout or cut body
  moves to the next route, billing the prompt twice with the first attempt
  recorded nowhere. Record the attempt, and fail over only on errors that
  mean the request was not taken.
- Unbounded buffers in the tier holding every key: `sse_payloads` on a line
  with no newline, whole error bodies in `from_response`
  (`crates/gateway/src/llm/provider/mod.rs`), and the Bedrock frame buffered
  to its declared total before the checksum is checked. Cap each.
- The private-address denylist (`crates/core/src/egress/rule.rs`) misses
  NAT64, 6to4, IPv4-compatible, Teredo and site-local ranges, and 198.18/15
  and 192.0.0.0/24. Replace it with "global unicast only"; a denylist keeps
  missing ranges.
- Fail-open controls: the breaker, cancel and the spend-cap hold all proceed
  when Postgres is unreachable (`router.rs`, `crates/core/src/breaker.rs`).
  Argued in comments; say it in the operator docs, since the kill switch is
  one of them.
- One shared runtime key for every pod. Sound as designed, but a leak takes
  every workspace's turns from outside the cluster. Per-pod keys issued at
  registration, or mTLS, when there is a second cluster to care about.
- Small: the Gemini translation ignores `reasoning_effort`.

### Shape of the code

- `crates/api/src/worker.rs` is projection, compaction and its HTTP, six
  unrelated sweeps and the turn lifecycle in one file, with `announce`
  duplicating `tell` and an action store built ad hoc where the others are
  injected. Split into `projection.rs`, `compaction.rs`, `sweeps.rs` and
  `turn.rs`.
- `crates/runtime/src/component.rs`: `AgentHost` has some fifty fields and one
  `Host` impl spans storage, model streaming, egress, timers and the clock.
  `stream_completion` is an SSE parser that belongs in `core` beside the
  gateway's protocol code. Split by capability with a thin dispatcher; the
  missing admission charges were invisible because of the accretion.
- `crates/gateway/src/llm/translate.rs`: each vendor's stream and non-stream
  paths are separate reimplementations. The usage branch collapses the usage
  and stop-reason pairs; the rest of each pair should follow.
- Errors are `(StatusCode, String)` (`crates/api/src/router.rs`): no code a
  client can match on, so the UI matches prose. A `{code, message}` body is
  cheap now and a migration later.
- `tool_name_of` in the default agent duplicates `operation_tool_name` in
  `crates/api/src/skill/mod.rs`; one should be generated from the other or
  shared through the WIT.
- `puller.rs` maps `max_tool_rounds <= 0` to unbounded and an empty
  `write_scopes` to `["session"]`: a setting that meant "none" becomes
  "everything". Make zero and empty mean what they say.

### The browser

- The transcript read is paged and reports `has_more`, but the client type
  omits it and nothing sends `before` (`ui/src/lib/chat.ts`, `History`).
  Older messages in a long session are simply absent. Add the page link.
- The phone nav and sessions drawers close on Escape but have no focus trap;
  the existing overlays have none either, so adding one is a decision for
  all of them.

### Build, CI and tests

- The agent component is a committed binary nobody rebuilds in CI
  (`assets/agent_default.wasm`, `Dockerfile`). Rebuild it in CI and fail if
  it differs from the commit, or build it in the image.
- No `cargo audit` or `cargo deny` step; two generations of the RustCrypto
  crates are linked at once. Add the step and align the pins.
- Images for production should be pinned by digest; everything first-party
  and third-party floats today, which is fine locally and nowhere else.
- `tests/api.rs` is thirteen thousand lines in one module. Split by resource,
  keeping `tests/common`.
- Tests that do not exist and should: a lease expiring during preparation; a
  crash between a turn's commit and its completion; a stale lease presented
  to `/work/gated` and `/work/wait`; the browser reconnecting after a fetch
  error, replaying a batch, switching sessions mid-stream and failing to load
  history.

## Tier 1 — independent

Things that need nothing above them.

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

Built: the wizard and its page, from a specification to a published skill
with a manifest, category files and a file per operation, and the key it needs
connected on the way. It is also what makes the fork below answerable with
evidence rather than argument.

And the premise now has evidence of its own. One run was written out by hand
for Hollowbrook on 2026-09-22 -- `k8s/components/hollowbrook/skill/` is what a
wizard run would produce -- and the agent read the detail file before calling,
every time, including after the pattern was obvious. The whole design fails if
a model guesses the call from a manifest line, so that is the assumption worth
having tested before building on it. See *The shape, tried once* in
[openapi-wizard.md](openapi-wizard.md).

### Skills as packages

[skill-packages.md](skill-packages.md). Built.

A skill version as a body plus a set of files, versioned together and read
through a `skill/` scope resolved against the version the turn bound. It
replaced detail kept in `workspace/` scope beside the skill: overwritten in
place while the body was versioned, owned by nothing, editable by anyone with
workspace storage write, and unreachable for an operator's skill. The wizard
writes into this shape.

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

### OAuth client credentials on egress rules

[client-credentials.md](client-credentials.md).

A rule naming a token URL, scopes and two variables, with the gateway
exchanging them for a bearer token cached per replica and refreshed on demand.
Built. The OpenAPI wizard prefilling such a rule from a `clientCredentials`
scheme is not.

### Credential bindings

[credential-bindings.md](credential-bindings.md).

A credential variable usable only by the workspaces and toward the hosts the
operator bound it to, enforced in the gateway. Built. Binding to an
organization rather than a list of workspaces waits on organizations.

### Sealed credentials

[sealed-credentials.md](sealed-credentials.md).

A credential pasted into a page, sealed there to the gateway's public key with
its binding -- workspace, hosts, header, token URL -- as associated data, and
stored in a table anyone may read and only the gateway can use. Replaces
`OUTTURN_EGRESS_` variables and their bindings, so adding a credential stops
being a manifest edit and a pod roll, and keeps the property credential
bindings were built for: nobody who can write the database can change where a
secret goes. Built for static header credentials, with a page to connect,
test, replace and revoke them; client-credentials pairs still name variables.
The environment path stays until no rule uses it.

### Derived skills

[openapi-wizard.md#a-derivation-not-an-output](openapi-wizard.md#a-derivation-not-an-output).

A generated skill as a function of a stored specification and annotations keyed
by operation -- notes, preferences, hidden operations, approval rules, observed
examples -- so updating the specification proposes a new version that keeps
everything people added, and reports what no longer applies. Agents still read
only published versions.

Built: annotations at three levels (skill, category, operation) -- notes,
preferences, hidden operations and approval rules -- kept beside the stored
specification and applied on every regeneration, which is a proposal until it
is published. A gate survives everything that could drop it quietly: a gated
operation is never hidden, adopting a skill made before specifications were
kept turns its hand-written rules into annotations, and a publish that would
drop a gate the live version has is refused unless told to. Not built:
detaching a skill from its specification, refreshing one from its URL, and
observed examples.

### Idempotency

[idempotency.md](idempotency.md).

**Declared writes are built** (2026-10-20): an operation names its identity,
the host derives a key from it, a recipient that takes one is sent it, and the
API's record covers one that does not. What follows is the crash-replay half
for writes nobody declared, which is still open.

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

[triggers.md](triggers.md). Schedules, webhooks and email (inbound parse)
built as kinds of one model; the single form, IMAP and attachments remain.

The only item here that makes agents useful to a workspace not sitting in the
UI, which is most of them most of the time.

Schedules are cheap because the queue was built for them: `jobs.run_after`
schedules work forward and the claim reads `priority` first, so background
turns cannot get in front of somebody waiting. Both exist and are tested.

Two things it surfaces rather than solves, both recorded in that document:
notifications, which is where a reply nobody asked for and a repeatedly failing
schedule both land, and a dashboard panel for agent health. Neither exists, and
triggers are what make their absence matter.

### Session search

[session-search.md](session-search.md). The sessions list becomes recent by
last activity, paged, and everything older is reached by search: titles by
trigram and content by full-text, both narrowed in the query. Embeddings are an
optional component on top, called through the gateway so they land in the
ledger like any other model call.

Built: the recent list, paged by last activity, read a page at a time by the
sidebar; title search by trigram, narrowing the same list; and a read of one
session for a link the list has not reached. Not built: content search and
embeddings. Content search is heavier than titles -- index user messages only,
capped per message, with the workspace in the index -- and the least valuable
of the three, so it waits until titles are not enough.

### ~~Confirm what inhibitors actually does~~ — done, 2026-09-20

Stopping is built and suspension is not, in the precise sense that matters:
**nothing can take a suspended hold.** Both endpoints hardcode
`Strength::Stopped` (`crates/api/src/router.rs`), so `Strength::Suspended` appears
only in the enum, its parser, the verdict mapping and unit tests. The
`Verdict::Suspended` arm in `worker::inhibited` is reachable only by a row
written straight to the database.

What that arm does when reached is refuse the turn without latching: no
requeue, no parked state, the job completes, and the next turn re-evaluates.
So a suspension does not park and resume today -- it declines, and something
must arrive later to try again.

That made human-in-the-loop a build rather than a wiring-up, and said what
the build was. Approvals since take suspended holds, and a suspended turn now
parks rather than declines (migration 0014); see its entry in tier 2.

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

Built. [approvals.md](approvals.md) and [action-queue.md](action-queue.md).

The three pieces this entry named are all there. Approvals and gates take a
suspended hold, so the request that needs a person is what asks for one. A
held turn parks rather than declines -- a `parked` job state the claim skips,
which leaves its session's queue free (migration 0014). And answering the
approval wakes it, so the turn runs again of its own accord. The inbox tells
somebody who is not looking at the session, which is what keeps an approval
from becoming a turn parked for ever.

### Auto-approval

[auto-approval.md](auto-approval.md). Built; what is left is listed under
*What is not built* there -- chiefly the admin agent proposing a policy, the
action queue saying a broad policy has operations waiting to be accepted, and a
token trade-in dropping a revoked policy rather than relying on the record.

A workspace deciding that some gated requests no longer need a person: policies
by risk, act or operation, conditions on the gate's bound fields, a trial mode
that only annotates the queue, evidence from the approval history, and a record
the gateway writes before the request goes out. Rests on approvals, which are
built. The decision first is the operator's half -- `risk`, `auto` and `numbers`
in frontmatter, and a cap that only narrows -- since a policy can only be as safe
as what it is allowed to cover.

## Tier 3 — needs tiers 1 and 2

### Integrations

[integrations.md](integrations.md).

Needs workspace-defined egress, and two things that document describes and
nothing implements: a credential store that holds one credential per workspace
with three possible lifecycles, and per-agent scoping, which is a schema change
plus a resolution rule that must narrow rather than widen.

The credential store is designed in
[integrations.md#authorization-code-grants](integrations.md#authorization-code-grants):
person- or workspace-owned grants, refresh tokens encrypted under a key only
the gateway holds, the callback with state, PKCE and a browser-bound cookie,
the grant id committed into the turn token, and revocation that reaches a
running turn. Unbuilt.

Not the platform egress list. A workspace integrating with Notion or a
customer's public API never touches the internal path.

Not tool registration. An integration is a permitted host, a bound credential,
and a skill saying what to call -- all of which `fetch_url` already serves.

### The admin agent

[admin-agent.md](admin-agent.md).

An agent a workspace administrator configures the workspace through: it
proposes, a person confirms, and the confirm endpoint is one its token cannot
reach. Its API credential is sealed to the gateway, forwarded only to
`/v1/manage/*`, and its turn carries no other egress. Needs sealed credentials
for that token, and is worth building once auto-approval and derived skills give
it something precise to write.

### Skill bundles

[skill-bundles.md](skill-bundles.md).

Several skills shipped, enabled and versioned as one thing, with the hosts they
reach and the credentials those need -- "Accounts Receivable" rather than any
one skill in it. The unit somebody actually wants, and the thing a workspace
assembles by hand today in a dozen steps where stopping halfway is worse than
not starting.

Wants integrations first, or most of it: a bundle whose hosts and credentials
a workspace must still arrange by hand is a bundle that cannot do its job. The
two are the same problem from opposite ends -- that document approaches it from
the credential, this one from the work.

Installing must be a plan a person approves rather than an act, since the
alternative is a catalog click that reaches a host and takes a credential.
The unsettled part is versioning: whether a workspace may edit a bundle's
skills, and what an upgrade does when they have.

Note this word changed meaning. It used to mean one skill and its files, which
is now a *package* -- see [skill-packages.md](skill-packages.md).

### Egress rules in the UI

No spec of its own; the rules are [egress.md](egress.md) and
[integrations.md](integrations.md).

Partly built: Settings > Connections lists the workspace's hosts, allows and
removes them, and connects each one's key. What follows was the case before it,
and still is for the agent and platform levels.

`/v1/egress-rules` exists and had no UI, so allowing a host meant an API call. That is the gap worth closing, and it is the workspace level: the table
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

## Done, and not from this list

Work that answered something found by using the thing rather than by reading
the plan. Recorded because the list above says nothing about it, and a reader
comparing the two would otherwise think it never happened.

**Skill operations as tools** — 2026-10-19/20. A skill's operations are
offered as tools, their requests built by the guest from what each file
declares ([skill-operations.md](skill-operations.md)) -- which retired the fork
"tool registration" used to stand here as, by registering nothing. Arguments
are JSON Schema, resolved by the OpenAPI wizard from a specification's types
into a skill's shared `types.json`; a real accounting API went from no tools to
377 of 385. Found by small models confusing `load_tools`, `read_object` and
`fetch_url`, and by walking the wizard through two real specifications, which
also turned up the header, body, content-type and key-test fixes now in.

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
model nowhere at all: `crates/api/src/skill/mod.rs` composes each skill into the
prompt as a `## {name}` heading and `ResolvedSkill` has no slug field. Both
were found by a person trying it, not by a test.

Since 2026-10-21 picking a skill invokes it rather than writing anything: the
composer shows it as a chip, the message carries its slug beside the content,
and the turn is told what was picked and handed the skill's main reference
file. See [prompt-contributors.md](prompt-contributors.md#a-skill-somebody-invoked).

**Dictation, and operator ceilings** — 2026-10-09. A microphone in the
message box using the browser's own speech recognition, on by default; a
setting of a new kind, a ceiling, which an operator can turn off for
everyone and a workspace can turn off for itself but not back on. See
[settings.md](settings.md#who-may-override).

**Feedback on replies and conversations** — 2026-10-09. A thumb up or down
on each of the agent's replies, about that reply or the whole conversation,
with an optional note: one verdict per person per target, seen by everyone who
may read the conversation. It is the flagging
[skill-evaluation.md](skill-evaluation.md) needs, as the correction channel
and as the only labeling that will ever happen; nothing reads it yet. Stored
per reply rather than per session, because a conversation can go well for ten
turns and break on the eleventh, and evaluation has to know which.

**Agent templates** — 2026-10-07. An agent the operator defines once and has
made in every workspace that should have it: required, default or optional;
a prompt in requirements and defaults, with a workspace's own part after
both; fixed settings that win over the workspace's; eager tools; and versions
a workspace may pin when the template allows it. See
[agent-templates.md](agent-templates.md).

**Hollowbrook as a component, and one wizard run by hand** — 2026-09-22.
`scripts/dev-mac.sh --with hollowbrook` brings the guesthouse up, opens its
host, and installs a skill describing its API -- so the platform's own
demonstration is one flag.

The installation is a Job calling the public API rather than anything in the
platform. The first version was a Rust module in `crates/api/src/` with the skill
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

**Reviewing what an import suggests, as a page of its own.** Driving the
wizard on a real specification (Bigcapital, about three hundred operations)
on 2026-10-25: *Accept all suggested* is the right idea and the wrong shape.
It writes every rule at once with no progress shown, so a long list sits
silent until it finishes; it reads as "approve everything" when what is
being asked is "confirm how risky each operation is"; and it is a section
at the foot of the skill's source rather than a step of its own. Wanted: a
review page that walks the suggestions in groups (reads, writes, money,
deletes), says plainly that each risk is a guess to confirm, shows progress
while it writes, and works on a phone, which nobody has checked yet. Plain
words throughout: an operator should not need to know what a gate, a bind or
a risk band is to finish it.

**Fixed headers on a connection.** Some APIs want a constant on every call
besides the credential -- an organization or tenant header, an API version.
A skill's operations leave every header out of their tools
([openapi-wizard.md](openapi-wizard.md), *Operations as tools*), which is right
when the specification over-declares one, as an accounting API's
"required if the token is a JWT" organization header did; but an API that
really needs one has nowhere to get it. The value is the workspace's -- its own
organization -- so it belongs on the connection beside the key: a list of
names and values, not sealed since none is a secret, attached by the gateway
from the committed egress rule so an agent cannot change them. The connection
could suggest the headers the bound skill's specification mentions and its
tools leave out, to be filled in only when a call fails without one.

**Channels: Slack, email and SMS as conversations.** An agent spoken to
through another medium, its reply sent back the way the message came and a
thread kept as one conversation. Built on a trigger source for the inbound
half ([triggers.md](triggers.md), *Triggers are not channels*), with sending
and the mapping of a sender to a person as its own. It also reopens who may
talk to an agent at all: a channel open to a tenant's customers is a seat
this platform has deliberately not had.

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

The other half of an inbox -- what is waiting on the reader rather than what
happened -- is [action-queue.md](action-queue.md), which is partly built. The
two are separate on purpose: an event stays true and is read or unread, while a
decision somebody owes is open until it is settled and leaves the queue when a
colleague answers rather than when the reader looks.

Not `QueueItemPrimitive`, which renders assistant-ui's own composer queue --
the lanes this project deliberately leaves empty, because a message is
persisted the moment it is sent and a browser-held copy would be a second
source of truth that vanishes with the tab. An inbox is the opposite case:
the server is the origin, so there is nothing local to disagree with it.

**Replaying what the model was thinking.** Showing it is built: reasoning is
stored as a part of the reply (`Part::Reasoning`) and drawn in the transcript.
What is left is the provider half. Anthropic's thinking blocks are still
dropped at `crates/gateway/src/llm/translate.rs` with their signatures, so a turn
continuing into tool use cannot return them, and nothing gates display: every
workspace sees reasoning where the model gave it. What a provider will accept
back is one question and what a person may see is another; only the second is
answered.

The real constraint is narrower: Anthropic wants a thinking block returned
with its signature when a turn continues into tool use, so the block and its
signature have to be kept together and replayed where the API expects them,
and dropped where a provider has no equivalent. That is the ordinary shape of
multi-provider mapping rather than a reason not to store them.

Two pieces remain: keep the block and signature, and replay or drop them per
provider at the gateway; and a workspace setting that gates display. The
second matters because thinking is less filtered than output and a model
sometimes reasons about things it does not say -- which is a reason for a
workspace to choose it deliberately, not a reason nobody may have it.

**A dashboard panel for agent health.** The smallest useful version of the
above, and it reads rows `/v1/usage` already carries.

**History access.** An agent reading its own past conversations, which a
schedule's "summarize yesterday" examples assume and nothing provides.
Designed in [history-access.md](history-access.md), and off unless somebody
turns it on: it is the one tool that could repeat one customer's conversation
to another.

**Memory, or whatever the durable thing turns out to be.** A schedule running
weekly in a fresh session each time cannot learn anything, and the fix is not a
long-lived session. What it wants is narrow and durable, and the shape is
unsettled: an agent writing notes for its future self is a judgment about what
mattered, which is nearer compaction carry-over than user-declared memory, and
[compaction.md](compaction.md) is explicit that those must not share a store. It is also partly an
evaluation problem, since knowing what was worth keeping means knowing what
went wrong without it.

**Models configured in the database.** Built through phase 7 of [models.md](models.md):
the catalog loaded on first launch, provider accounts, models and chains
edited in the UI with sealed keys, settings per model, tracking the latest
version without breaking, and first-run recipes. It replaces the routes page
this entry used to describe, and its explicit fallback row: a chain is that
fallback, drawn. What remains is Bedrock, a workspace's own models, and an
agent naming a chain of its own.

## Designed, and not yet tiered

Each has a design or its reasoning written down, and is waiting on a place in
the tiers rather than on a decision.

### Prompt contributors

[prompt-contributors.md](prompt-contributors.md).

One shape for everything that shapes a turn besides the conversation -- the
preamble, the agent's prompt, skills, a voice, a stance, tools loaded and
operations read up front, the time a conversation started -- each with an
owner, a placement, a priority, a budget and a reach, recorded per turn. The
system prompt is composed once per conversation and again only at compaction
-- built, the first piece, with compaction on request (`/compact`, and a
banner when a conversation is behind what is published) -- and that is what a
catalog of installable contributors would be built on.

**Instructions as data, and experiments on them.** Undesigned. A system
prompt is a list of instructions, each with its own reason, and trying one
should be a switch rather than a code change and a deploy. One contributor
holding instructions as rows -- text, placement, and when it applies: always,
never, per model, or one variant of an experiment -- would make each one
something an operator turns on where it earns its place. Two details decide
whether it measures anything:

- *Sticky per conversation.* A conversation has one system prompt, so it has
  one variant, chosen when it starts and kept: switching between turns
  costs the cache and drops thinking blocks as stale. Chosen at random, or
  from a hash of the person who started it, so a person always sees the same
  one -- the second is the stratifying choice, at the cost that the variant
  follows who starts conversations.
- *Recorded on each reply,* with who took part, so a comparison is a query
  against outcomes already stored (`feedback`) rather than reading
  transcripts.

The first candidate is the preamble's rule that instructions come from the
system prompt alone (d5765c5). Tried by hand, two runs a case, it moved both
models the same way: an override styled as a new system prompt, asking for
a pirate poet, held 0 of 2 on qwen3.5 and 1 of 2 on Sonnet 5.5 without it,
and 1 of 2 and 2 of 2 with it; one asking for recipes as part of the job
held 1 and 2 of 2 without, 2 and 2 with; "shorter, please" was followed
every time. So it is on for every model until an operator's own numbers say
otherwise -- the knowledge of which models need it is the deployment's, not
a list shipped here.

Open beside it, for when several people can share a conversation: whose
`person` contributions -- a voice -- apply, since a conversation has one
prompt. The starter's, probably, and settled once for both features.

### Personalities

[personalities.md](personalities.md).

A person's own voice for an agent -- terse, Bleargh, UwU, or being called
pathetic -- layered over the agent's prompt for them alone. Composed last, so
it is lowest priority and the shared prefix stays cacheable; confined to replies
to that person, never to work done for anybody else; none on turns nobody is
waiting on. Recorded per turn, so evaluation can measure what a voice costs in
accuracy rather than averaging it away. Needs nothing above it.

### A time zone for `get_current_time`

No design of its own; the reasoning is here.

The tool answers in the person's zone, from their browser, and deliberately
takes no zone argument: a model allowed to name one would invent a plausible
wrong one. Places keep their own zones -- a guesthouse's, each resort property's
-- and those belong to the business system's data or the skill's own notes,
which is where a skill's curator says them ("each property's `timezone` field
is authoritative"). Outturn keeps no clocks of its own beyond the person's.

What a note cannot give an agent is the time *there now*: it has to work it out
from the person's time and an offset, and models get that wrong across a
daylight-saving change. The fix is an optional `timezone` argument taking IANA
names only (`America/Cancun`, never "Mexico time"), refused when it is not a
real zone, described as "only one you read from data or were told". Without
it, the tool answers as today. Waits for a skill that needs it; it changes the
agent component, so the committed `agent_default.wasm` and its bindings are
rebuilt with it.

### Terms: third-party models

Not code. Customers should be told that the models their agents use belong
to someone else, whose terms apply too -- and outturn has a wrinkle a generic
clause misses: a workspace may use the operator's provider keys or bring its
own ([routing.md](routing.md)), and the clause has to cover both. Linked from
the site and from workspace sign-up, and read by a lawyer before it is relied
on. The wording to start from, as agreed:

> **Third-party AI models.** Agents on this platform use AI models provided by
> third parties such as Anthropic, Google and OpenAI. Your use of those models
> through the platform is subject to the provider's terms and usage policies,
> as well as ours. Where you connect your own provider account or key, your
> agreement with that provider governs that use directly. We may suspend
> activity that would breach a provider's policies.

Each provider's usage policy is linked by name rather than paraphrased, since
those change. The last sentence is the one the platform can already keep: a
workspace can be held ([inhibitors.md](inhibitors.md)).

**Compute in the usage ledger.** The ledger records tokens for every model
call and nothing about compute. A turn is given `FUEL_PER_TURN`, and what it
spent is logged but recorded nowhere -- so a guest that burns most of its
budget on every turn costs CPU that reaches no bill. Work done by services on
a turn's behalf, a PDF render among them, is not fuel at all and would be
metered by the service that did it. Fuel is
the unit: deterministic across pods, counted outside the guest's memory, and
read with one subtraction however the guest ended. Wanted: a sibling table to
`usage_ledger` keyed per turn, with the same dimensions, and
the figure carried back from the runtime with the turn's result. Two things to
settle first. Host-side work done on a guest's behalf burns no guest fuel,
so this meters the guest's share only. And it would be the first ledger figure
the runtime vouches for rather than the gateway, which belongs in
"What a compromised runtime still reaches" in invariants.md. Budgets a
customer feels stay in spend controls, priced; per-turn fuel stays a runaway
guard. See [pdf-rendering.md](pdf-rendering.md).

## Known costs, left until they matter

Found by a read-only audit of what each list query loads, and narrowed since;
what remains is cheap today and grows with something specific. Each says what
it grows with, so the trigger to come back is a number rather than a feeling.

**A skill's history is built from whole versions.** The browser receives
summaries now, but the API builds them by loading every version on the page --
prose, every file row and its links -- and keeping the counts and the changed
files. Up to a hundred versions of a generated skill is tens of thousands of
rows read to send a few hundred bytes. Grows with versions × files. The fix is
the file count and the per-version changes computed in SQL, with neither the
prose nor the links read for the list.

**The agents list reads what it no longer sends.** The roster dropped each
agent's system prompt and policy from its response, but the query behind it
still selects them and the handler throws them away. Grows with prompt length
× agents in a workspace, which is small. The fix is a list query and type of
its own in the agent store.

**The transcript is not virtualised.** A page of it is bounded, so this is a
very long session scrolled far back. Grows with how much of one session
somebody reads in one sitting.

## Not on this list

Recorded so their absence is deliberate.

**Compaction carry-over.** Designed in [compaction.md](compaction.md). Independent
of everything here, and the trim beneath it already guarantees a turn never
fails for context.

**An end-customer surface.** A guest signing in to talk to an agent is a
different product with different auth, and
[integrations.md](integrations.md) explains why the deployment this platform is
for does not need one. The row-level authorization it would require is not on
this path.

**Redis caching, OpenTelemetry, workflows as scripted tasks in
sub-sessions.** [design-notes.md](design-notes.md#direction) lists these as intended. None
blocks anything above.
