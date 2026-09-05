# Working on outturn

outturn is a multitenant agent platform: tenants deploy agents that serve their
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
| `gateway` | Talks to model providers. Holds the credentials; nothing else does |
| `runtime` | Runs agent components in a WASM sandbox |

The split is a security boundary, not a packaging one. An agent runs in the
runtime with no filesystem, no sockets and no credentials — every capability it
has is an explicit host import declared in `wit/agent.wit`. When it wants a
model it calls `chat`, and the host attaches the token. A compromised agent can
spend its session's allowance and nothing more.

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
| `OUTTURN_MAX_IN_FLIGHT_TURNS` | api | Turns one pod claims before it stops claiming |
| `OUTTURN_DEFAULT_MODEL` | api, runtime | Model when an agent names none |

An idle runtime pod always accepts a turn however tight memory looks. Without
that, a pod whose baseline sits under the reserve refuses everything forever,
because no turn is running whose ending could change the answer.

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

## Invariants worth knowing before you change things

These are load-bearing. Each has already caused a visible bug.

**Streamed deltas concatenate to stored content.** What the browser renders
during a turn must be exactly what the transcript holds afterwards, or the
message changes under the reader when the turn ends. Every round of a tool loop
streams, so the guest returns the content of all of them.

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
tenant, and an empty list is the default -- a tenant who has not thought about
it has not consented to it. The tenant's list is checked first and the resolved
address second, and the second check is not theirs to waive: an allowed name
that resolves inside the cluster is still refused. Names are resolved once and
the connection pinned to the answer, or the check and the request are about
different places. Redirects are not followed, because a redirect names a host
nobody checked.

**Credentials are named, never stored.** A rule carries the name of an
environment variable; the host reads it and attaches the header on the way out.
The guest cannot read it and cannot set the headers it travels in. Nothing that
reads `egress_rules` can leak a secret by reading it, which is why the table is
safe to return to a browser.

**Refusing work is not failing it.** A runtime pod at capacity answers 503,
and the worker returns the job with `jobs::release`, which gives back the
attempt the claim counted. Map that 503 onto `jobs::fail` and a cluster that is
merely busy will exhaust a turn's retries without ever running it, and tell the
user their turn failed because the service was popular. Releases are counted
separately and give up past `MAX_RELEASES`, so a permanently full cluster
reports rather than spins.

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
code: Redis caching, pull-based session assignment, per-tenant usage
attribution, OpenTelemetry, and workflows as scripted tasks in sub-sessions.

A tenant's egress list is managed through `/v1/egress-rules` and has no UI
yet, so allowing a host means an API call.

The gateway must eventually support mid-session provider failover — an
Anthropic outage substituting Gemini and continuing. That requires separating
the durable transcript from the projection sent to a model, so provider-specific
artifacts (Gemini thought signatures, Anthropic thinking blocks, differing tool
call shapes) are annotations filtered per target rather than facts about
storage. Lossy parts should degrade, never fail the turn.
