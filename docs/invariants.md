# Invariants

Load-bearing rules: each has already caused a visible bug when broken. Read the
one you are near before you change things.

These are load-bearing. Each has already caused a visible bug.

**A job state is enumerated in more places than the schema.** `cancelled` was
added to the check constraint and the two SQL lists that decide whether a turn
is retried, and both of the others were missed on the first pass: the guard
that decides whether an empty reply was abandoned, which wedged a session
permanently the moment a turn was stopped before its first token, and the
union the browser switches on, which showed nothing at all. A cancelled turn is
*accounted for*, so it belongs in every list meaning "this turn finished" — and
it is terminal and never retried, so it must not appear in any list meaning
"give this back to the queue". Grep for the state strings, not just the
constraint.

**Streamed deltas concatenate to stored content.** What the browser renders
during a turn must be exactly what the transcript holds afterwards, or the
message changes under the reader when the turn ends. Every round of a tool loop
streams, so the guest returns the content of all of them, joined with a blank
line. The blank line is streamed by the *host*, before the later round's first
token: a guest only learns a round produced text when `chat` returns, and a
separator sent then lands after the text it was meant to precede -- which is
how two replies once arrived glued together with a stray blank at the end.

**A turn's conversation stops at its own prompt.** A message the user sent
after the prompt is already stored when the turn is prepared; left in the
history it reaches the model twice, once as history and again as a steer, and
the model answers the later message in the earlier one's reply. `up_to` in the
worker is what enforces this.

**The transcript read returns its own cursor.** History and the event cursor
come from one statement so they share a snapshot: everything at or below the
cursor is already in the content, everything above is still to come. Read them
separately and a reload replays deltas into content that already contains them
— a message appends itself.

**A reply hangs off the prompt it answers, once per attempt.**
`agent_messages.replies_to` and `attempt`, unique together. A turn creates its
reply empty and streams into it; when a worker dies mid-generation the job is
retried, and the constraint makes the retry take back the reply it already made
rather than orphaning it. Ownership is on the prompt rather than the session
because two turns in one session run concurrently and must not claim each
other's.

The attempt is what lets a turn stopped for a person keep what it said. A crash
retries its *own* attempt and takes the row back, because the first try produced
nothing anybody saw. A turn resumed after an approval asks for the next one
instead: the refusal it was stopped on is the thing the approver approved
against, and taking the row back overwrote it -- leaving a transcript that showed
a charge succeeding with a green "approved" beside it and nothing in it that ever
needed approving. `chat::attempt_for` is the one place that decides which of the
two this is, because two call sites claim the same placeholder for one turn and a
number computed twice can differ.

What decides is whether the attempt *said* anything, not whether it finished.
The rule used to keep only a finished attempt, reasoning that a crash had
produced nothing anybody saw -- which a streaming turn makes false. One ran for
eight minutes, made six tool calls, had its lease reaped, and the retry took the
row back and overwrote all of it with a one-sentence answer: the calls were on
the reader's screen while they happened and then were not. `said_something` is
the question, the same predicate the discard and the abandoned guard ask.

Asked of the row alone it answers wrong for exactly that case: a reply's row
is written once, when it finishes, so every attempt still streaming reads as
empty and what it said is only in its events. `attempt_for` therefore seals an
interrupted attempt from its events -- the same `replay` a reload mid-turn
uses -- and marks it `interrupted` before moving on. Only `prepare_turn` may
ask it; anything after that asks `current_attempt`, because a turn mid-stream
looks exactly like one interrupted and deciding again would seal it.

An attempt that never *finished* is taken back even when resuming: an empty reply
stranded past a new attempt is what the abandoned-placeholder guard trips over,
and that wedges the session.

**A reply's parts are one rule, written once.** `api::chat::parts` holds the
kinds and the builder that assembles them, and both the live path and `replay`
go through it -- they produced the same shape by two transcriptions of one rule
until they did not. `Part` is an enum, so a kind added later is a compile error
at every consumer rather than a silent drop in the projection that builds a
model's request; an unrecognized kind decodes to `Unknown` so a rolling deploy
can still read its own rows. Two copies remain and cannot be collapsed: the
browser's, in `ui/src/lib/useChatRuntime.ts`, and the SQL predicate
`said_something`, which both `discard_placeholder` and the
abandoned-placeholder guard call. Changing what counts as an empty reply means
changing all three.

**A request has one shape.** `egress::grant::Shape` is the four values every
tier identifies an outbound request by -- method, host without its port, path,
body -- and `Shape::of_fetch_arguments` is the only host-side reading of a
stored call. The guest that builds the request from a model's arguments is
another crate compiled to wasm, so it cannot share the code; a test pins the two
readings to a real stored call instead. If `fetch_url` learns to accept an
object body or an aliased url key, that test is what should fail -- the
alternative is that nothing fails and approved calls quietly stop being
retracted.

**Ordering rides on UUIDv7 keys.** No sequence columns, no offset pagination.
Cursors are the last id seen; `Uuid::nil()` means the beginning.

**Streaming calls need a read timeout, not a total one.** A generation running
for minutes while producing tokens is fine; silence is not. TCP keepalive
catches a dead peer in about a minute, but a peer that is alive and silent is
invisible below the application layer — and the job heartbeat renews the lease
while a worker waits, so nothing else would ever reclaim it.

**An agent reaches nothing it was not allowed.** Egress rules name hosts, per
workspace, and an empty list is the default -- a workspace who has not thought about
it has not consented to it. The workspace's list is checked first and the resolved
address second, and the second check is not theirs to waive: an allowed name
that resolves inside the cluster is still refused. Names are resolved once and
the connection pinned to the answer, or the check and the request are about
different places. Redirects are not followed, because a redirect names a host
nobody checked. All of it happens in the gateway, which is where the request is
made from; `src/runtime/egress.rs` still holds the rule matching and the
address vetting, and the gateway calls it.

**The agent interface has generated files committed beside it.** Change
`wit/agent.wit` and `assets/agent_default.wasm` and `agents/default/src/bindings.rs`
must be rebuilt with it, or every turn fails in the linker with a mismatch a
file comparison catches first -- which `artifact_guard` does. `wit/renderer.wit`
is the same with `assets/pdf_renderer.wasm` and `renderers/pdf/src/bindings.rs`,
guarded the same way; a renderer that does not link stops the runtime at
startup rather than failing the first PDF. The toolchain and
the exact flags are in the README; the flags are what reproduce the committed
files byte for byte, so do not change them casually.

**The runtime signs nothing.** It executes workspace components, so it holds no
key that could mint a credential for anyone: it presents `OUTTURN_RUNTIME_KEY`,
which is compared in constant time and means only "the runtime tier", and the
API mints the gateway token each turn travels with. Giving the runtime the
signing secret would let a compromised component's host mint `system_admin`.

**An agent's requests are made by the gateway, not by the runtime.** The
runtime executes workspace code, so it holds no credentials and has no outbound
HTTP path of its own: it asks the gateway, presenting the turn token the API
minted for it. The gateway reads the egress commitment out of that token,
checks the rule the caller offered against it, vets the address, attaches the
credential and makes the call. A runtime that rewrote its own copy of the rules
gets nowhere, because the copy it can rewrite is not the one consulted -- and
an approval it was merely trusted to honor would be worth nothing, since a
compromised runtime would simply not ask.

Two things follow that are easy to undo by accident. Nothing in the runtime may
decide whether a request is allowed: a check performed by the sandbox's own
host is the thing being defended against rather than the thing defending, and
`runtime/fetch.rs` is a client with no policy in it on purpose. And the cluster
should say the same thing the code does -- `k8s/base/networkpolicy.yaml` denies
the runtime any egress but the API, the gateway, minio and DNS, so a host that
grew a socket still reaches nothing. It needs a CNI that enforces
NetworkPolicy; kind's default does not.

**The commitment is what makes a rule a rule.** The API hashes a turn's egress
rules into one root -- `src/egress/commit.rs`, one hash whatever the list's
length -- and signs it into the turn token. A request carries the rule it wants
and a proof, which is the whole set for a short list and an inclusion path for
a long one, and the gateway rebuilds the root and compares. The empty set has a
tag of its own, because a stripped claim must never read as "this workspace
allows nothing", and a token with no commitment is refused rather than given
the benefit of the doubt. What the scheme cannot do is prove absence: a host is
refused by failing to be proven allowed, which is why "could not verify" must
always mean refused.

**What a compromised runtime still reaches.** It holds the turn tokens of the
turns it is running, so it can act as those tenants: their allowed hosts, with
their credentials attached by the gateway, and their responses. That is the
boundary -- co-residency, not the platform. It cannot obtain a credential, act
for a workspace whose turn it is not running, or reach a host nobody allowed.
Narrowing it further is a scheduling decision rather than a code one: do not
put turns from different tenants on one pod.

**A turn token outlives nothing, but is replaced before it lapses.** It is
minted for thirty minutes, and a turn has no upper bound while it works --
onboarding in long-horizon mode can run for hours. So the runtime checks before
every gateway call and, with under fifteen minutes left, trades the token in at
`POST /v1/work/{job_id}/token` under its lease. The API copies the commitments
out of the presented token rather than recomputing them: the runtime carries the
rules the turn started with, and a token committing to different ones would fail
every request. So removing a host does not reach into a turn already running.
Only a still-good token is accepted; there is no path that reads an expired one.
Not a heartbeat, deliberately: a check tied to calls means a stopped or parked
turn simply never asks.

**A token is good for one audience.** Browser tokens carry `outturn:api`,
turn tokens carry `outturn:gateway`, and each validator insists on its own.
Before this, a turn token was a working API credential for its workspace and an
Operator's cookie a working gateway one -- the roles differed, the verifier did
not. Turn tokens also carry `Role::Turn`, which holds `GatewayInvoke` alone.
The subject claim is a user id in the first kind and a chat session id in the
second, and the audience is what says which.

**Credentials are named, or sealed -- never stored in the clear.** A rule
either names a sealed credential ([docs/sealed-credentials.md](sealed-credentials.md),
built for static headers): a secret sealed in the browser to the gateway's
public key, its binding as associated data, stored in a table only the gateway
can open, with no restart to add one. Or, the older path, it carries the name of an
environment variable; the host reads it and attaches the header on the way out.
The guest cannot read it and cannot set the headers it travels in. Nothing that
reads `egress_rules` can leak a secret by reading it, which is why the table is
safe to return to a browser. Writing one is the other half: a workspace writes its own rules,
so a rule may name only a variable under `OUTTURN_EGRESS_`, never one of the
gateway's own secrets. The gateway checks this where it reads the variable,
not only where the API stores the rule.

The prefix is shared by every workspace, so it is not the whole answer: a
variable is used only by the workspaces and toward the hosts (and token URL)
the operator bound it to in `OUTTURN_CREDENTIAL_BINDINGS` on the gateway. The
binding lives beside the secret rather than in a table, so not even the API
chooses where a secret goes; unbound, or a declaration that will not parse,
means refused. See [docs/credential-bindings.md](credential-bindings.md).

That includes a token a rule exchanges client credentials for. It is a working
credential for as long as it lasts, so it lives in the gateway's memory, one
cache per replica, and not in the database the breaker shares -- where the API
and anyone holding its password could read it. See
[docs/client-credentials.md](client-credentials.md).

**Runtimes take work; nothing is pushed to them.** A pod with room polls
`/v1/work` for one turn and is given one, so a full pod is never offered work it
would have to refuse. Pushing meant guessing which pod had capacity: measured
over 120 turns it cost 692 refusals, and the turns that kept losing that lottery
waited fourteen seconds to start while others began in a tenth of one.

**A lease is the only thing joining a claim to the runtime running it.** The
tier handing work out claims the job; the runtime reports what it produced. If
that pod dies, nothing fails the job -- the thing that would have is gone -- so
the lease is what recovers it, renewed while results arrive and reaped when they
stop. Remove either half and turns are lost or run twice: without the reaper a
crashed runtime blocks its session for ever, and without renewal a turn longer
than the lease is handed to a second pod while the first is still streaming.
The lease token travels with the assignment and comes back in `x-outturn-lease`
on every report and hand-back, and completing, failing and releasing all check
it -- so a pod whose lease lapsed cannot write over the pod that now holds it.

**A claim must skip keys that are already running before it applies its
limit.** Runtimes ask for one turn at a time. If the candidate query took the
top pending row and only then asked whether its session was busy, a session
with one turn running and one queued would be the top candidate on every
claim, be rejected on every claim, and nothing behind it would ever be looked
at -- one person sending two messages froze dispatch for the whole cluster.
There is a test for this; keep it passing.

**The sandbox caps guest memory; admission only estimates it.** Admission
decides whether to start a turn from the memory that is free, and charges a
flat `ASSUMED_TURN_BYTES` for its lifetime. Nothing about that stops a
component from growing once it is running, so `GUEST_MEMORY_LIMIT` is enforced
by wasmtime's store limiter. Remove it and a component can `memory.grow` to
four gigabytes and take the pod, and every turn on it, with it.

**A person waiting comes before scheduled work.** Jobs carry a priority and
the claim reads it before `run_after`, so a backlog of background work cannot
put itself in front of somebody watching a reply. It cannot preempt a turn
already running -- the guarantee is that the next slot to free anywhere in the
fleet goes to the higher priority, which is bounded by the shortest turn in
flight rather than by how long a pod takes to start.

**Capacity is estimated ahead of the queue, not from it.** Queue depth is a
lagging measure: by the time work is queued somebody is already waiting, and a
pod arriving thirty seconds later does not help the turns that queued. So
`desired_runtime_pods` is a floor plus a term for recently active sessions --
which predict arrivals that have not happened yet -- plus terms for waiting
work at each priority. The autoscaler reads it with a target of one, because
the arithmetic belongs in a view somebody can read rather than smuggled into a
threshold.

**Scale on work that could start, not work that is waiting.** A serial key
admits one running job at a time, so a session with a hundred queued turns is
one unit of work. The `job_backlog` view is the one statement of that, and a
test holds it to what `claim` actually takes; counting rows asks for pods that
cannot claim anything. Relatedly, the deployments KEDA manages carry no
`replicas:` — a count in the manifest is a standing instruction to undo the
autoscaler on every apply.

**A pod is killed against its cgroup, not the node.** And against its working
set, not `memory.current`: page cache is charged to the cgroup and stays
charged until there is pressure, so usage climbs to the limit and never comes
back. Subtract inactive file cache, or a pod that has read some files reports
itself permanently full.

**Pods can silently predate your edits.** When behavior contradicts the
source, check pod age before theorising.
