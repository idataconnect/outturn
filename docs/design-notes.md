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

Storage layout and retention are written up separately, in
[docs/storage.md](storage.md) — including why the object prefixes are
ordered scope-first rather than as the hierarchy you would expect, which looks
like a mistake until you know about S3's per-bucket lifecycle rule cap.

How an agent reaches anything outside the platform -- and why the tier running
workspace code holds no credential and opens no socket -- is in
[docs/egress.md](egress.md). It also has the design for reaching a service
inside the cluster, which is refused today -- an escape hatch rather than the
usual path, since a customer's service normally has a public name and needs
none of it. Where one genuinely does not, the guard that stops a workspace
aiming the gateway at `outturn-api` stops that too, and the operator's list is
what separates them.

Who supplies those hosts and the credentials that go with them is a separate
question, designed and unbuilt in [docs/integrations.md](integrations.md):
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
level is in [docs/settings.md](settings.md). Both are mostly design: each
says what exists.

Starting a turn when nobody is typing is in [docs/triggers.md](triggers.md)
— schedules and webhooks being built, email designed and deferred. An inbound
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
(notes, preferences, hidden operations, approval rules; observed examples are
still to come) -- so an updated specification proposes a new version that keeps
everything people added and reports what no longer applies. Agents still read only published
versions; annotations never reach a turn. In the same document, under "A
derivation, not an output".

A skill as a body plus files versioned together -- a *package* -- is in
[docs/skill-packages.md](skill-packages.md), built. A
version carries its files, a fork copies them, and an agent reads them as
`skill/<slug>/<path>` from the version its binding resolved to.

Several skills shipped and versioned as one thing -- a *bundle*, "Accounts
Receivable" rather than any one skill in it -- is designed and unbuilt in
[docs/skill-bundles.md](skill-bundles.md). It wants integrations first:
a bundle whose hosts and credentials a workspace must still arrange by hand
cannot do its job.

How somebody finds out their skill is not working is designed but unbuilt, in
[docs/skill-evaluation.md](skill-evaluation.md) — a skill that documents
its call the way an API's own docs do ("GET https://…") reads fine to a capable
model and gets called as a tool name by a weaker one, and nothing today would
say so. Most of that is a query rather than an inference; the judged half reads
untrusted transcripts, which is the part to be careful with.

Finding a conversation again is in
[docs/session-search.md](session-search.md), partly built: the recent list
by last activity rather than every session ever made exists, paged in the
sidebar and searchable by title, and so does `/v1/agents/activity` beside it.
Still designed only: searching message content, and embeddings as an optional component like Tika. They go through the gateway
rather than beside it: an embedding is a model call, and a model call that
skipped the ledger would be the first.

What happens to a write nobody saw the answer to is designed but unbuilt, in
[docs/idempotency.md](idempotency.md) — a tool call has three outcomes
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
belongs to; what it still points at is an event rather than the hold, and it
carries a lifecycle column the hold should own. That document settles which is
the truth and what changes when suspension lands. Nothing should start reading
`action_items.state` as the answer to whether a request is open.

A refusal a turn parked on is retracted when it resumes. The gate answers the
guest with "Do not retry this request", which is right while the turn is
stopping and wrong once somebody says yes -- it is a tool result, so it replays
to the turn whose whole purpose is to make that call, and the model obeys the
last thing in its own transcript. `worker::answered` swaps that one sentence on
a resuming turn; the refusal itself stays, being what the approver approved
against. It is safe only because the gate refuses *before* dispatch, so the call
provably never went out. A gate that refused after would make this a retry of a
call that may have landed -- see "What approvals already assume" in
[docs/idempotency.md](idempotency.md).

A round cut partway is therefore untrusted in full: its tool calls may carry
arguments truncated mid-JSON, and running one is precisely the outcome that
document is about. They are refused the same way a reply cut off at the token
limit has always been refused — see the truncation guard in
`agents/default/src/lib.rs`.

Letting a workspace stop being asked about a gate it trusts the agent with is
designed and unbuilt in [docs/auto-approval.md](auto-approval.md):
policies over the operator's declared `risk` and `auto` ceiling, conditions
only on the gate's bound fields -- never the agent's own judgment -- decided at
the gateway and recorded there before the request goes out. Configuring that,
and the rest of a workspace, through an agent is in
[docs/admin-agent.md](admin-agent.md), and its one rule is that the agent
proposes and a person confirms through an endpoint its token cannot reach.

Words this codebase uses in a particular way are in
[docs/glossary.md](glossary.md) -- worth a look before a design
conversation, since a few are easy to misread — and two of them recently swapped.
A *package* is one skill shipped as a body plus its files; a *bundle* is several
skills shipped together. Until recently "bundle" meant the first, so anything
written before the swap that says "bundle" for one skill means *package*.

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
