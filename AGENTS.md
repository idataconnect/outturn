# Working on outturn

outturn is a multiworkspace agent platform: workspaces deploy agents that serve their
own customers, with isolation, usage attribution and security boundaries built
in rather than added later. Rust, Axum, Tokio, PostgreSQL, WASM sandboxing,
Kubernetes. Apache-2.0, edition 2024.

This file is for anyone — human or agent — picking the project up. It records
what is true today, what is intended, and the traps that have already cost
someone a night.

## The tiers

Three binaries, deployed as three services:

| binary | does |
|---|---|
| `api` | HTTP API, auth, transcripts, the job queue and its worker |
| `gateway` | Talks to model providers, and makes an agent's outbound requests. Holds the credentials; nothing else does |
| `runtime` | Runs agent components in a WASM sandbox |

The split is a security boundary, not a packaging one. An agent runs in the
runtime with no filesystem, no sockets and no credentials — every capability it
has is an explicit host import declared in `wit/agent.wit`. When it wants a
model it calls `chat`, and the host attaches the token. When it wants a URL the
host asks the gateway, because the tier running workspace code is the wrong
place to hold a credential or to decide what may be reached. A compromised
agent can spend its session's allowance and nothing more.

The gateway speaks *protocols*, not vendors. `provider/openai.rs` is the OpenAI
chat-completions protocol, which ollama, Groq, OpenRouter and most others also
speak — they differ in base URL and credential, which is configuration. Only
Anthropic earns its own file, because its wire format genuinely differs. Adding
a vendor should not mean adding a file.

## Running it locally

Start the cluster with the Control API open, so a build and deploy can be
triggered without hitting Enter:

```bash
skaffold dev --auto-build=false --auto-deploy=false --auto-sync=false --rpc-http-port=50052
curl -X POST http://localhost:50052/v1/execute -d '{"build":true,"deploy":true}'
```

Build and deploy must go in **one** request; a lone deploy can softlock the
loop (skaffold #4886). `--trigger=manual` on its own does not work — it gates
file watching, not the API. Check `buildState.autoTrigger` in `/v1/state`:
`true` means `/v1/execute` returns `{}` and silently does nothing.

Skaffold forwards 18080 (api), 18081 (gateway), 18082 (runtime) and 15432
(postgres), and keeps them alive across redeploys. **Do not start your own
`kubectl port-forward`** — it will not reconnect, and it pushes skaffold onto
different ports without saying so.

The UI runs outside the cluster:

```bash
cd ui && npm run dev     # :3000, proxies /v1 to localhost:18080
```

The proxy mounts the API at `/v1`, matching production. Do not introduce a
prefix: the refresh cookie is `Path`-scoped to `/v1/session/refresh`, and a
browser matches `Path` against the URL it requests, not the one a proxy
forwards. A `/api` prefix means refresh silently never works.

Local dev seeds `admin@outturn.local` / `outturn-dev`.

## Models

Local development runs against ollama through the OpenAI protocol
(`OPENAI_BASE_URL`, no key). **Use gemma4.** The agent offers tools on every
turn, and a model whose template stops streaming when tools are present
collapses a reply to three chunks — llama3.1 and mistral both do this, gemma4
does not. It is per-model template behaviour, not an ollama or gateway
property. olmo-3 cannot do tools at all.

On a Mac, `scripts/dev-mac.sh` runs the `mac` profile instead: ollama on the
host through `host.docker.internal`, and **qwen3.8:27b-mlx** as the default.
It passes both checks above -- it streams with tools offered, and unlike gemma4
it still answers a tool result with thinking off.

A local model does not know what it is, and will not say so. Asked what
model it was, an agent claimed to be Claude; asked again after a change, it
denied being qwen and named a product that does not exist, describing this
platform's file scopes and tools accurately around the invented name. Neither
was a prompt going wrong -- nothing told it what it was, so it wrote the
likeliest continuation, and being challenged moved it rather than correcting
it. Every turn's prompt now opens with the product, the version and the model
actually serving it (`skill::compose_for_turn`), which is what a question about
itself now finds. `OUTTURN_BRAND_NAME` is the deployer's own name for this
tier, matching the UI's `VITE_BRAND_NAME`, which is compiled into the browser
bundle and never reaches the API.

That fixes identity and nothing else: the same reply invented HTTP headers to
check. Small local models embellish, and a prompt can only supply facts it has.

Thinking is on by default with tools. `reasoning_effort: "none"` turns it off
where supported and cuts a gemma4 tool turn from ~113 completion tokens to 24.
It hangs off the agent's policy, beside `model`.

## Tunables

Environment variables, all optional, all with defaults in the code beside the
constant they replace.

| Variable | Tier | Bounds |
|---|---|---|
| `OUTTURN_MAX_CONCURRENT_TURNS` | runtime | Turns one pod carries before answering 503 |
| `OUTTURN_MEMORY_RESERVE_BYTES` | runtime | Working-set headroom kept clear of the cgroup limit |
| `OUTTURN_API_URL` | runtime | Where a runtime asks for work |
| `OUTTURN_DEFAULT_MODEL` | api, runtime | Model when an agent names none |

Three are required rather than tunable, and each tier gets only the one it
needs:

| Variable | Tier | Is |
|---|---|---|
| `OUTTURN_TOKEN_SECRET` | api only | Ed25519 seed, 64 hex chars. Signs every token |
| `OUTTURN_TOKEN_PUBLIC_KEY` | api, gateway | Its public half. Comma-separate two during a key rotation |
| `OUTTURN_RUNTIME_KEY` | api, runtime | Shared key the runtime presents to take work, 32+ bytes |

An idle runtime pod always accepts a turn however tight memory looks. Without
that, a pod whose baseline sits under the reserve refuses everything forever,
because no turn is running whose ending could change the answer.

Runtime pods are 512Mi in base and in the local overlay alike, so what is
learned locally about admission carries over. On SIGTERM a runtime stops
asking for work and waits for the turns it holds, up to the deployment's
`terminationGracePeriodSeconds`; KEDA's `cooldownPeriod` does not protect a
turn mid-generation, it only governs scaling to zero.

## Changing the agent interface

`wit/agent.wit` is the boundary between the host and the components it runs,
and two generated files are committed beside it: `assets/agent_default.wasm`,
so an image build needs no wasm toolchain, and `agents/default/src/bindings.rs`,
so the guest crate builds without one either. Neither regenerates on its own.

Change the interface without rebuilding them and the guest still compiles --
against its stale bindings -- while every turn fails at runtime with "component
imports instance `outturn:agent/host`, but a matching implementation was not
found in the linker". A test catches it first: `artifact_guard` compares the
interface against the copy stored beside the component, and fails the moment
they differ.

Rebuilding needs a toolchain that is not otherwise required:

```bash
rustup target add wasm32-wasip2
rustup component add llvm-tools          # rust-lld needs libLLVM to link a component
cargo install wit-bindgen-cli --version 0.41.0

wit-bindgen rust wit/ --out-dir agents/default/src --runtime-path wit_bindgen_rt --format
mv agents/default/src/agent_world.rs agents/default/src/bindings.rs
(cd agents/default && cargo build --release --target wasm32-wasip2)
cp agents/default/target/wasm32-wasip2/release/outturn_agent_default.wasm assets/agent_default.wasm
cp wit/agent.wit assets/agent_default.wit
```

The bindgen flags are not a guess and should not be changed casually: they are
what reproduces the committed file byte for byte. A cheap way to confirm before
trusting a regeneration is to run them against the *old* interface and diff
against the committed bindings -- identical means the flags are right, and
anything else means the next diff will be full of noise that hides the change.

## Tests

`cargo test` runs what is fast and needs nothing. Use it while working.

Before a push, run everything:

```bash
TEST_DATABASE_URL='postgres://outturn:outturn-dev@localhost:15432/outturn_test' \
GATEWAY_URL=http://localhost:18081 \
cargo test --features integration-tests
```

Two gates. `integration-tests` is for suites that need services -- Postgres,
a gateway, a model -- and it implies `slow-tests`, which is for suites gated by
what they cost rather than what they need. The component suite needs no
services at all but instantiates a sandbox per case and saturates the machine,
which is a poor trade against a check run twenty times an hour.

A third, `live-providers`, is implied by neither and never should be. It runs
`tests/provider_live.rs`, which calls real providers on real keys and spends
real money:

```bash
GEMINI_API_KEY=... cargo test --features live-providers gemini_
```

**A key in the environment is not permission to spend it.** Somebody who
clones this, sets the keys the documentation tells them to set, and runs the
suite while finding their way around must not discover afterwards that we
billed them for it. The flag is the consent, and it has to be typed. Anything
that reaches a paid endpoint belongs behind it -- and nothing else may imply
it, including whatever gets run before a push.

Within that suite a missing key fails rather than skips, because a test runner
gives a skipped case no louder a voice than a passing one, and "all passed"
when nothing ran is worse than a red line. Run one provider at a time by
filtering on the name.

Do not assert elapsed time in the slow suites. Sixteen sandboxes compete for
whatever cores are left, and a turn there finishes when the suite does rather
than when its own deadline fires -- a test that measured this failed about half
the time while the deadline it was testing worked perfectly. Bound the work
from outside with `tokio::time::timeout` and assert that it finished.

Every test gets a private Postgres schema, so the suite is safe to run in
parallel and no test has to clean up after another. `tests/common/fake_gateway.rs`
serves scripted responses — including a tool call, a truncated stream and a
connection that hangs — so the tiers above the provider can be tested for
behaviour rather than for whatever a model happened to say. Only
`tests/agent_component.rs` needs a live model.

Two things are declared in `k8s/base` but do nothing until asked. The mock
provider (`outturn-mockllm`) sits at zero replicas: it is what integration and
end-to-end tests point at when the shape of a turn matters and the words do
not, so it belongs where CI cannot forget to install it. Everything it serves
carries an `x-outturn-mock` header, so a transcript it produced can be told
apart from a real one later by somebody who does not know it exists.

KEDA's manifests are *not* in base, and the difference is not arbitrary. A
deployment at zero replicas costs one object; a controller with CRDs and
webhooks cannot be installed inertly. So `k8s/autoscaling` is applied
deliberately, after `helm install keda`, and local development runs without it.

Who may do what is written up in [docs/authorities.md](docs/authorities.md):
authorities are the fixed vocabulary in code, roles are workspace-owned rows that
bundle them, a token carries role names only, and the API resolves them on
every request through a per-workspace cache invalidated over LISTEN/NOTIFY.

Tenancy, whose credential pays and what is attributed are written up in
[docs/tenancy.md](docs/tenancy.md) — the short version being that `workspace_id`
is the isolation boundary and stays that way, with organizations added above it
rather than nesting beneath it.

Storage layout and retention are written up separately, in
[docs/storage.md](docs/storage.md) — including why the object prefixes are
ordered scope-first rather than as the hierarchy you would expect, which looks
like a mistake until you know about S3's per-bucket lifecycle rule cap.

Every model call is a row in the usage ledger, tagged with workspace, agent,
session, user, the workspace's own account label, the model that actually served,
and whose key paid; the export at `/v1/usage` is what bills are built from.
Written up in [docs/usage.md](docs/usage.md).

Which model answers, whose key pays and how fallback works across workspaces that
bring their own keys is in [docs/routing.md](docs/routing.md); how defaults
cascade from operator to workspace to agent with an explicit override at each
level is in [docs/settings.md](docs/settings.md). Both are mostly design: each
says what exists.

How somebody finds out their skill is not working is designed but unbuilt, in
[docs/skill-evaluation.md](docs/skill-evaluation.md) — a skill that documents
its call the way an API's own docs do ("GET https://…") reads fine to a capable
model and gets called as a tool name by a weaker one, and nothing today would
say so. Most of that is a query rather than an inference; the judged half reads
untrusted transcripts, which is the part to be careful with.

What happens to a write nobody saw the answer to is designed but unbuilt, in
[docs/idempotency.md](docs/idempotency.md) — a tool call has three outcomes
rather than two, and the third one, sent-but-never-observed, is what a stop
button has to avoid creating.

There is a stop button now, and it avoids it by never stopping inside a round.
`POST /v1/agent-sessions/{id}/cancel` records the request on the job row; the
gateway sees it within `CANCEL_POLL` and cuts the provider stream, which closes
the upstream connection and is the only thing a provider understands as "never
mind"; the guest hears about it through `limits.cancelled` and returns at its
next round boundary, keeping whatever it had written.

A round cut partway is therefore untrusted in full: its tool calls may carry
arguments truncated mid-JSON, and running one is precisely the outcome that
document is about. They are refused the same way a reply cut off at the token
limit has always been refused — see the truncation guard in
`agents/default/src/lib.rs`.

## Invariants worth knowing before you change things

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

**A reply hangs off the prompt it answers.** `agent_messages.replies_to`, with
a unique index. A turn creates its reply empty and streams into it; when a
worker dies mid-generation the job is retried, and the constraint makes the
retry take back the reply it already made rather than orphaning it. Ownership
is on the prompt rather than the session because two turns in one session run
concurrently and must not claim each other's.

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
an approval it was merely trusted to honour would be worth nothing, since a
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

**A token is good for one audience.** Browser tokens carry `outturn:api`,
turn tokens carry `outturn:gateway`, and each validator insists on its own.
Before this, a turn token was a working API credential for its workspace and an
Operator's cookie a working gateway one -- the roles differed, the verifier did
not. Turn tokens also carry `Role::Turn`, which holds `GatewayInvoke` alone.
The subject claim is a user id in the first kind and a chat session id in the
second, and the audience is what says which.

**Credentials are named, never stored.** A rule carries the name of an
environment variable; the host reads it and attaches the header on the way out.
The guest cannot read it and cannot set the headers it travels in. Nothing that
reads `egress_rules` can leak a secret by reading it, which is why the table is
safe to return to a browser.

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

**Pods can silently predate your edits.** When behaviour contradicts the
source, check pod age before theorising.

## Direction

Intended but not yet built, so that nobody mistakes these for facts about the
code: Redis caching, per-workspace usage attribution, OpenTelemetry, and workflows
as scripted tasks in sub-sessions.

A workspace's egress list is managed through `/v1/egress-rules` and has no UI
yet, so allowing a host means an API call.

There is no per-workspace fairness in the queue, on purpose for now. Priority
classes put a waiting person ahead of background work; within a class, order
is arrival. One workspace's burst can therefore sit in front of another's until
the autoscaler catches up, and the bet is that it catches up fast enough for
this not to matter. If that bet fails, the fix is a fairness term in the
claim's ordering, which means changing the claim index and the backlog view
together.

### Compaction

Not built. The design, so it is not rediscovered:

A transcript outlives any model's window, and the window is a property of the
route rather than of the session — so a turn can arrive at a smaller context
than the one before it. The naive ordering, compact in the outgoing model
before switching, is a trap: it bills the user for an expensive operation they
did not ask for, at the moment they asked for something else.

Summarise from the system prompt, the previous summary, and the tail. That
input is bounded by construction, so the incoming model can always do it
however long the session has run, and no compaction depends on a model that is
being switched away from. The tail is also where the live context is: what is
being worked on now, the recent tool results, the thread of the conversation.

Summaries are cumulative — each one summarises the tail plus the summary before
it — because the alternative loses durable facts. Constraints stated once at the
start are exactly what gets dropped and then violated. Cumulative carrying is
not a guarantee, only a much better chance; the real fix is memory, an explicit
write that says "this survives", so compaction does not have to guess what was
load-bearing. Memory makes compaction safer, which is an argument for building
it second rather than first.

A summary is a message a model wrote about the conversation and will be
replayed on every later turn, so its failure mode is quiet: a summary that
misstates a decision becomes the record. Mark it as a summary in the
transcript rather than folding it in as ordinary history — both so a reader can
see what happened, and so the next compaction knows it is compacting a summary.

Underneath all of it, a trim that cannot fail: drop whole turns from the oldest
end until the prompt fits. No model call, so it works when the provider is
down, the breaker is open, or the summary itself would not fit. It is what
guarantees a user never sees "context exceeded", which is the actual
requirement — everything above is about doing better than that.

Two things the trim must not get wrong. **Never split a tool turn**: dropping an
assistant message carrying tool calls while keeping its results produces a
request both protocols reject, trading one hard error for another, so the unit
of dropping is a turn including its round trips. And **always keep the last user
message**, since a turn with nothing to answer is already an error in the guest.

Budgets belong on the route, beside `model`, because that is the thing that
varies. Compact against a fraction of the window rather than the whole of it,
leaving room for the reply, for tool results arriving mid-turn, and for the
compaction call itself.

The gateway must eventually support mid-session provider failover — an
Anthropic outage substituting Gemini and continuing. That requires separating
the durable transcript from the projection sent to a model, so provider-specific
artifacts (Gemini thought signatures, Anthropic thinking blocks, differing tool
call shapes) are annotations filtered per target rather than facts about
storage. Lossy parts should degrade, never fail the turn.
