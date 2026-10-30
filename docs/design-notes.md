# Design notes

A tour of how each part of the platform works and what is built, with links to
the document that designs it. The roadmap says what is next.

Who may do what is written up in [docs/authorities.md](authorities.md):
authorities are the fixed vocabulary in code, roles are workspace-owned rows that
bundle them, a token carries role names only, and the API resolves them on
every request through a per-workspace cache invalidated over LISTEN/NOTIFY.

Tenancy, whose credential pays and what is attributed are written up in
[docs/workspaces.md](workspaces.md) — the short version being that
`workspace_id` is the isolation boundary and stays that way, with organizations
added above it rather than nesting beneath it.

Running a release rather than a checkout -- the images CI publishes once it
has passed, `k8s/deploy` pointed at an external Postgres and object store, an
infrastructure repository pinning a commit, and the order an upgrade rolls
the tiers in -- is [docs/deployment.md](deployment.md).

Storage layout and retention are written up separately, in
[docs/storage.md](storage.md) — including why the object prefixes are
ordered scope-first rather than as the hierarchy you would expect, which looks
like a mistake until you know about S3's per-bucket lifecycle rule cap. How
much a workspace may hold, and how that is counted as files are written and
corrected weekly from the bucket, is [docs/storage-quota.md](storage-quota.md).

How an agent reaches anything outside the platform -- and why the tier running
workspace code holds no credential and opens no socket -- is in
[docs/egress.md](egress.md). It also has the design for reaching a service
inside the cluster, which is refused unless the operator names the host in
`OUTTURN_INTERNAL_HOSTS` -- an escape hatch rather than the
usual path, since a customer's service normally has a public name and needs
none of it. Where one genuinely does not, the guard that stops a workspace
aiming the gateway at `outturn-api` stops that too, and the operator's list is
what separates them.

Who supplies those hosts and the credentials that go with them is a separate
question, in [docs/integrations.md](integrations.md). Part of it is built: a
skill declares its hosts and someone who may write egress rules opens them in
one step, with client credentials, credential bindings
([docs/credential-bindings.md](credential-bindings.md)) and sealed credentials
([docs/sealed-credentials.md](sealed-credentials.md)) to authenticate. The
rest is designed:
the operator installs their own APIs, approves extensions a workspace can turn
on for itself, and may optionally let a workspace add its own hosts -- which is
today's behavior, so that tier is a restriction to add rather than a feature.
None of the three waits on a new tool. `fetch_url` already reaches any host a
workspace is allowed, so an integration is a permitted host, a credential bound
to it, and a skill saying what to call -- and the work is in the rules and the
credential rather than in the guest. A typed tool would be checked by the same
code as a `fetch_url` call to the same place, so it adds no containment; what
it would add is argument shape and argument constraints, which
[docs/roadmap.md](roadmap.md) records as a fork to take when an
integration needs them.

Every model call is a row in the usage ledger, tagged with workspace, agent,
session, user, the workspace's own account label, the model that actually served,
and whose key paid; the export at `/v1/usage` is what bills are built from.
Written up in [docs/usage.md](usage.md).

Which model answers, whose key pays and how fallback works across workspaces that
bring their own keys is in [docs/routing.md](routing.md); how defaults
cascade from operator to workspace to agent with an explicit override at each
level is in [docs/settings.md](settings.md). Settings are built; routing is
part built and part replaced by models.md.

Which models a deployment has, and what each accepts, is
[docs/models.md](models.md), built -- Bedrock included, tested against
stand-ins but not yet against AWS: a catalog from models.dev
shipped in the image and checked weekly; provider accounts with sealed keys,
models pinned or tracking the latest in their line, and a chain per traffic
class, all configured on Platform → Models or made by a recipe on Platform →
Setup with a test call; settings chosen per model; an upgrade the settings
would not survive waiting in the operator's inbox; and a note under any reply
a fallback answered. The gateway takes each model's facts from the catalog
rather than its name. How a model's reasoning
is asked for, shown, kept and given back is [docs/thinking.md](thinking.md);
asking, showing and keeping are built. Prompt caching marks the system prompt
and the newest message for Anthropic ([docs/caching.md](caching.md)); a long
conversation is summarized rather than truncated, and the system prompt is
spent rather than compacted ([docs/compaction.md](compaction.md)).

Starting a turn when nobody is typing is in [docs/triggers.md](triggers.md)
— schedules, webhooks and inbound email are built; email through IMAP is
designed and unbuilt. An inbound
delivery is authenticated by `hmac` or by `shared_secret`, and under `hmac` it
is recorded so the same signed request cannot be spent twice: the record is
anchored to the signed timestamp rather than to arrival, and released again on
any path that does not start a turn, so a delivery the ceiling refused can
still be retried. `shared_secret` is exempt, because it binds no time and a
record keyed on token and body would refuse a sender's legitimate duplicate.

Two labels rather than one: the *owner* who set a trigger up is recorded for
accountability, while `user_id` stays null because nobody is waiting, which is
also what stops an agent clearing its own stopped-session latch.

Turning an API specification into a skill is built, designed in
[docs/openapi-wizard.md](openapi-wizard.md) — a manifest in the prompt and
a file per operation in the object store, because a skill body is paid for on
every round of every turn and a real specification is megabytes. The agent
reads an operation with `read_object` when it needs one, which is the same
trade `load_tools` already makes for the guest's own tools.

The wizard and its page are built, and what it makes is a *derivation* -- a stored specification plus annotations keyed by operation
(notes, preferences, hidden operations, approval rules and write identities;
observed examples are still to come) -- so an updated specification proposes a new version that keeps
everything people added and reports what no longer applies. Agents still read only published
versions; annotations never reach a turn. In the same document, under "A
derivation, not an output".

A skill as a body plus files versioned together -- a *package* -- is in
[docs/skill-packages.md](skill-packages.md), built. A
version carries its files, a fork copies them, and an agent reads them as
`skill/<slug>/<path>` from the version its binding resolved to. A skill's
operations are offered to the model as tools of their own
([docs/skill-operations.md](skill-operations.md)), and an agent made from an
operator's template keeps the operator's instructions while a workspace adds
its own ([docs/agent-templates.md](agent-templates.md)).

Several skills shipped and versioned as one thing -- a *bundle*, "Accounts
Receivable" rather than any one skill in it -- is designed and unbuilt in
[docs/skill-bundles.md](skill-bundles.md). It wants integrations first:
a bundle whose hosts and credentials a workspace must still arrange by hand
cannot do its job.

How somebody finds out their skill is not working is mostly unbuilt, in
[docs/skill-evaluation.md](skill-evaluation.md). What exists is a person's
verdict on a reply or conversation, and per-skill statistics on the dashboard.
A skill that documents
its call the way an API's own docs do ("GET https://…") reads fine to a capable
model and gets called as a tool name by a weaker one, and nothing today says so
on its own. Most of that is a query rather than an inference; the judged half reads
untrusted transcripts, which is the part to be careful with.

Finding a conversation again is in
[docs/session-search.md](session-search.md), partly built: the recent list
by last activity rather than every session ever made exists, paged in the
sidebar and searchable by title, and so does `/v1/agents/activity` beside it.
Still designed only: searching message content, and embeddings as an optional component like Tika. They go through the gateway
rather than beside it: an embedding is a model call, and a model call that
skipped the ledger would be the first.

What happens to a write nobody saw the answer to is part built, in
[docs/idempotency.md](idempotency.md): the invocation record and declared
write identities exist, and replay from the record; the read-back probe does
not — a tool call has three outcomes
rather than two, and the third, sent-but-never-observed, is what a crash
creates however carefully a turn is written. Not what a stop creates: stopping
only happens at a round boundary, and a round cut partway has its tool calls
refused wholesale, so the deliberate path avoids the window rather than
recording it.

There is a stop button now, and it avoids it by never stopping inside a round.
`POST /v1/agent-sessions/{id}/cancel` records the request on the job row; the
gateway sees it within `CANCEL_POLL` and cuts the provider stream, which closes
the upstream connection and is the only thing a provider understands as "never
mind"; the guest hears about it through `limits.cancelled` and returns at its
next round boundary, keeping whatever it had written.

Stopping work for a reason other than somebody clicking stop -- a spend cap, an
operator, a turn waiting on an approval -- is in
[docs/inhibitors.md](inhibitors.md). Zero or more holds, each contributing
`suspended` or `stopped`, with the strongest winning and the verdict derived
rather than stored. Stopping is built: the strength and verdict model,
`decide`, a Postgres store, `/v1/inhibitors`, and enforcement in the worker and
the gateway.

Suspension now parks. A suspended verdict moves the job to `parked` -- a sixth
state -- dropping its lease so the reaper leaves it, and giving back the attempt
it spent so a turn suspended repeatedly does not fail for want of retries.
Releasing the hold calls `jobs::resume_parked`, scoped the way the hold was, and
the turn runs again and re-evaluates every hold: one another hold still covers
parks a second time, which costs a claim and is the right way round to be wrong.
`prepare_turn` returns `Prepared::{Run, Nothing, Park}` rather than an `Option`,
so the call site cannot fold parking back into "nothing to do" -- which is what
made `resumable` a promise with nothing behind it.

A suspended hold is taken by `api::gated`, when the gateway refuses a request a
skill declared as needing approval. The stop endpoints still write
`Strength::Stopped`, which is right: an operator stopping a workspace is not
asking a question.

A hold taken while a turn is *already streaming* is the case `prepare_turn`
cannot see, since that turn was claimed before the hold existed. The guest hears
about it at its next round boundary, finishes what it holds and returns; the job
then parks rather than completing, keeping the reply it wrote, so answering the
approval has a turn to give back. Such a turn must not latch its session --
latching is what makes a stop need a person, and an approval is lifted by one, so
latching would leave the conversation stopped after the yes. `awaiting_approval`
on the turn's result is what tells them apart.

`parked` is a job state, so it is enumerated in the places the invariant below
warns about: the accounted-for list in `chat::postgres` (left out, the first
message sent while an approval is pending wedges the session), the stop button's
lookup and its index, `request_cancel` (cancelled outright, like a pending job --
no runtime holds it), and the browser's `job_state` union, where it renders as
`held` so a reload says what the live `chat.held` event said -- and since, the
SQL function `live_turn` that says what a session's live turn is doing, and the
predicate of `jobs_live_turn_workspace_idx`, which `sessions::agent_activity`
repeats word for word to be served by it. `job_backlog` is deliberately
untouched: a parked turn cannot be claimed, so counting it would
ask for pods to run work nobody can take.

What declares that an operation needs approving, and what a yes is worth once
given, is in [docs/approvals.md](approvals.md) -- the rule lives in the
skill file that documents the operation, as frontmatter, because
skill-packages.md made those files versioned, immutable and writable only under
`SkillsWrite`. Who gets asked once a request is pending, and how they find out,
is in [docs/action-queue.md](action-queue.md) -- partly built, ahead of the advice
in inhibitors.md that said to wait for something to notify about. The queue
exists with role-valued targeting and one read across every workspace a person
belongs to. An approval's item names its hold through `inhibitor_id`. Its
`state` column is still read as whether a request is pending -- the second copy
of the hold's lifecycle that action-queue.md argues should go.

A refused call is held whole, and an approved one is sent by the platform. The
gate answers the guest with "Do not retry this request", which is right while
the turn is stopping; once somebody says yes, the next turn's runtime sends the
held request through the gateway before the guest runs, and the model reads
"Approved by ... and sent:" and the response where the refusal was -- it never
writes the approved call again, so it cannot write a different one. A request
that never left is tried again; one that may have landed goes back to a person
unless the recipient dedupes it. See *The approval carries the call* in
[docs/approvals.md](approvals.md), and "What approvals already assume" in
[docs/idempotency.md](idempotency.md).

A round cut partway is therefore untrusted in full: its tool calls may carry
arguments truncated mid-JSON, and running one is precisely the outcome that
document is about. They are refused the same way a reply cut off at the token
limit has always been refused — see the truncation guard in
`agents/default/src/lib.rs`.

Letting a workspace stop being asked about a gate it trusts the agent with is
built in [docs/auto-approval.md](auto-approval.md):
policies over the operator's declared `risk` and `auto` ceiling, conditions
only on the gate's bound fields -- never the agent's own judgment -- decided at
the gateway and recorded there before the request goes out. How a workspace
sets it -- one rule per gated operation, in one table and one panel, from
settings, the skill or the question itself -- is
[docs/approval-rules.md](approval-rules.md). Configuring that,
and the rest of a workspace, through an agent is in
[docs/admin-agent.md](admin-agent.md), and its one rule is that the agent
proposes and a person confirms through an endpoint its token cannot reach.

Words this codebase uses in a particular way are in
[docs/glossary.md](glossary.md) -- worth a look before a design
conversation, since a few are easy to misread — and two of them recently swapped.
A *package* is one skill shipped as a body plus its files; a *bundle* is several
skills shipped together. Until recently "bundle" meant the first, so anything
written before the swap that says "bundle" for one skill means *package*.

Files are more than bytes once a turn reads them. A document's text is
extracted by an optional Tika component and read in its place
([docs/document-extraction.md](document-extraction.md)); an agent writes a
PDF from Markdown through an optional renderer
([docs/pdf-rendering.md](pdf-rendering.md)); a phone's HEIC photo becomes a
JPEG at upload ([docs/heic.md](heic.md)); and an image is described by a
model that can see it rather than handed to one that cannot
([docs/vision.md](vision.md)). Large files are read and written in parts
([docs/streaming-storage.md](streaming-storage.md)). Everything besides the
conversation that shapes what a model is given -- the system prompt, tools
loaded up front, an invoked skill -- is a prompt contributor
([docs/prompt-contributors.md](prompt-contributors.md)), composed once per
conversation. An agent reading its own past conversations is designed and
unbuilt ([docs/history-access.md](history-access.md)).

## Direction

What is next and what blocks what is in [docs/roadmap.md](roadmap.md),
which links each item's design where one exists. The short version is that
little blocks anything: an integration is a permitted host, a bound credential
and a skill saying what to call, and `fetch_url` already serves all three. It
also records why tools living inside the guest is a fork to take later rather
than the prerequisite it first looked like.

Intended but not yet built, so that nobody mistakes these for facts about the
code: Redis caching, OpenTelemetry, and workflows as scripted tasks in
sub-sessions.

A workspace's egress list is managed through `/v1/egress-rules`, and from
Settings > Connections, which allows and removes hosts and connects each one's
key. The agent and platform levels have no UI.

There is no per-workspace fairness in the queue, on purpose for now. Priority
classes put a waiting person ahead of background work; within a class, order
is arrival. One workspace's burst can therefore sit in front of another's until
the autoscaler catches up, and the bet is that it catches up fast enough for
this not to matter. If that bet fails, the fix is a fairness term in the
claim's ordering, which means changing the claim index and the backlog view
together.
