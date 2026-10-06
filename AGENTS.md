# Working on outturn

outturn is a multitenant agent platform: workspaces deploy agents that serve
their own customers, with isolation, usage attribution and security
boundaries built in rather than added later. Rust, Axum, Tokio,
PostgreSQL, WASM sandboxing, Kubernetes. Apache-2.0, edition 2024.

## The tiers

Three binaries, deployed as three services:

| binary | purpose |
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

The gateway speaks *protocols*, not vendors. `src/gateway/llm/provider/openai.rs`
is the OpenAI chat-completions protocol, which ollama, Groq, OpenRouter and most
others also speak — they differ in base URL and credential, which is
configuration. Two vendors earn their own file: Anthropic, because its wire
format genuinely differs, and Gemini, because its OpenAI-compatible endpoint
folds away the cached and thinking token counts the usage ledger is built from,
so the native API is dialled and the translating done here. Adding a vendor
should otherwise not mean adding a file.

## Running it locally

Full detail, and the traps behind each line, in
[docs/local-development.md](docs/local-development.md). A machine that has
never run this: `.agents/skills/onboarding/SKILL.md` checks the requirements.

```bash
scripts/dev.sh             # skaffold dev with the Control API; --with tika,hollowbrook adds components
scripts/build.sh           # another terminal: one build-and-deploy round
cd ui && npm run dev       # :3000, proxies /v1 to localhost:18080
scripts/dev-secrets.sh --print   # the seeded admin@outturn.local password
```

- Build and deploy go in **one** request; a lone deploy can softlock skaffold.
- Skaffold forwards 18080 (api), 18081 (gateway), 18082 (runtime), 19000 (minio) and 15432
  (postgres). **Never start your own `kubectl port-forward`**: it won't
  reconnect and pushes skaffold onto other ports.
- Keep the API at `/v1` behind the proxy; a prefix breaks session refresh,
  whose cookie is path-scoped.
- Use **qwen3.5** through ollama locally, with thinking on; off makes its tool
  calling erratic. On a Mac, `scripts/dev-mac.sh`.
- When behavior contradicts the source, check pod age: pods can predate your
  edits.

## Tunables

Environment variables, all optional. All but one have defaults in the code
beside the constant they replace; the model does not, because a model written
into the code is one nobody chose, answering every turn of a deployment that
forgot to set one.

| Variable | Tier | Bounds |
|---|---|---|
| `OUTTURN_MAX_CONCURRENT_TURNS` | runtime | Turns one pod carries before answering 503 |
| `OUTTURN_MEMORY_RESERVE_BYTES` | runtime | Working-set headroom kept clear of the cgroup limit |
| `OUTTURN_API_URL` | runtime | Where a runtime asks for work |
| `OUTTURN_DEFAULT_MODEL` | api | Model when an agent names none. Unset, such an agent refuses its turns and sessions go unnamed |

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

## Tests

`cargo test` runs what is fast and needs nothing. Use it while working.

Before a push, run everything, but only if your change might have caused a bug
that can be caught by additional tests:

```bash
TEST_DATABASE_URL='postgres://outturn:outturn-dev@localhost:15432/outturn_test' \
GATEWAY_URL=http://localhost:18081 \
TEST_S3_URL=http://localhost:19000 \
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

**An LLM key in the environment is not permission to spend it.** Somebody who
clones this, sets the keys the documentation tells them to set, and runs the
suite while finding their way around must not discover afterwards that we
billed them for it. The flag is the consent, and it has to be typed. Anything
that reaches a paid endpoint belongs behind it -- and nothing else may imply
it, including whatever gets run before a push.

Within that suite a missing key fails rather than skips.

Do not assert elapsed time in the slow suites because of core competition
and parallel runs.

One test does, and is flaky for it: `long_poll_returns_empty_on_timeout` in
`tests/queue_and_events.rs` fails roughly one run in three under the full
suite and passes alone. It pauses tokio's virtual clock and then asserts the
poll waited out 25 seconds, and `EventBus::spawn` opens its listener on a task
of its own -- so a loaded machine can pause the clock while that connection is
still being made, and the first query races a clock already past the deadline.
Pre-existing, and not something a warm-up query fixes: tried, and it failed
twice in four runs rather than once in three. Fixing it properly means not
asserting on the clock, which is what the line above says. Re-run before
believing a red suite.

Every test gets a private Postgres schema, so the suite is safe to run in
parallel and no test has to clean up after another. `tests/common/fake_gateway.rs`
serves scripted responses — including a tool call, a truncated stream and a
connection that hangs — so the tiers above the provider can be tested for
behavior rather than for whatever a model happened to say. Only
`tests/agent_component.rs` needs a live model.

Two things are declared in `k8s/base` but do nothing until asked. The mock
provider (`outturn-mockllm`) sits at zero replicas: it is what integration and
end-to-end tests point at when the shape of a turn matters and the words do
not, so it belongs where CI cannot forget to install it. Everything it serves
carries an `x-outturn-mock` header, so a transcript it produced can be told
apart from a real one later by somebody who does not know it exists.

Scale it to one and set what it should pretend to be:

```bash
kubectl scale deployment outturn-mockllm --replicas=1
kubectl set env deployment/outturn-mockllm \
    MOCK_TTFT_MS=0 MOCK_TOKENS_PER_SEC=100000 MOCK_REPLY_TOKENS=24 \
    MOCK_TOOL_CALLS=0 MOCK_LOG_BODIES=1
```

`MOCK_TTFT_MS` and `MOCK_TOKENS_PER_SEC` are worth setting deliberately: at the
defaults a turn takes seconds, and a test driving a dozen of them spends
minutes waiting for a model that is pretending anyway. A slow setting has its
own use -- a stream long enough to interrupt is how the mid-turn stop is
tested. `MOCK_DROP_RATE` and `MOCK_HANG_RATE` make it misbehave on purpose.

`MOCK_LOG_BODIES=1` logs every request in full. That is the only way to see
what actually reached the model: the database says what a turn produced, and
inferring the prompt from it is how a marker or an instruction gets asserted
against somebody's idea of what was sent rather than what was.

KEDA's manifests are *not* in base, and the difference is not arbitrary. A
deployment at zero replicas costs one object; a controller with CRDs and
webhooks cannot be installed inertly. So `k8s/autoscaling` is applied
deliberately, after `helm install keda`, and local development runs without it.

## Conventions

Spelling is American English everywhere -- code, comments, docs and anything
a person reads: *summarize*, *color*, *behavior*, *catalog*. Two exceptions,
both because the word is stored rather than written: `cancelled` and
`cancelling` are job states and event kinds in the database and in applied
migrations, which cannot change (both spellings are accepted in American
English anyway). And never edit an applied migration to fix one: sqlx
checksums them.

Commits are small and say why, in the same prose voice as the code's comments.
Keep this file to hints that apply everywhere; a design, or the story of a
bug, goes in `docs/` with a line here pointing at it.

## Invariants worth knowing before you change things

Load-bearing; each has already caused a visible bug. The full text, with what
went wrong, is in [docs/invariants.md](docs/invariants.md) -- read the one you
are near before changing it.

- A job state is enumerated in more places than the schema.
- Streamed deltas concatenate to stored content.
- A turn's conversation stops at its own prompt.
- The transcript read returns its own cursor.
- A reply hangs off the prompt it answers, once per attempt.
- A reply's parts are one rule, written once.
- A request has one shape.
- Ordering rides on UUIDv7 keys.
- Streaming calls need a read timeout, not a total one.
- An agent reaches nothing it was not allowed.
- The agent interface has generated files committed beside it.
- The runtime signs nothing.
- An agent's requests are made by the gateway, not by the runtime.
- The commitment is what makes a rule a rule.
- What a compromised runtime still reaches.
- A turn token outlives nothing, but is replaced before it lapses.
- A token is good for one audience.
- Credentials are named, or sealed -- never stored in the clear.
- Runtimes take work; nothing is pushed to them.
- A lease is the only thing joining a claim to the runtime running it.
- A claim must skip keys that are already running before it applies its limit.
- The sandbox caps guest memory; admission only estimates it.
- A person waiting comes before scheduled work.
- Capacity is estimated ahead of the queue, not from it.
- Scale on work that could start, not work that is waiting.
- A pod is killed against its cgroup, not the node.
- Pods can silently predate your edits.

## Where things are written up

- [design-notes.md](docs/design-notes.md) -- how each part works and what is
  built, with links to its design. Start here before a design conversation.
- [invariants.md](docs/invariants.md) -- the rules above, in full, with the bug
  each one came from.
- [roadmap.md](docs/roadmap.md) -- what is next and what blocks what.
- [glossary.md](docs/glossary.md) -- words this codebase uses in a particular
  way. A *package* is one skill with its files; a *bundle* is several skills.
- [compaction.md](docs/compaction.md) -- summarizing long conversations, the
  trim underneath, and why the system prompt is spent rather than compacted.
- Access and tenancy: [authorities.md](docs/authorities.md),
  [workspaces.md](docs/workspaces.md), [settings.md](docs/settings.md).
- Models and spend: [routing.md](docs/routing.md), [usage.md](docs/usage.md),
  [caching.md](docs/caching.md).
- Reaching the outside: [egress.md](docs/egress.md),
  [integrations.md](docs/integrations.md),
  [sealed-credentials.md](docs/sealed-credentials.md),
  [credential-bindings.md](docs/credential-bindings.md),
  [client-credentials.md](docs/client-credentials.md).
- Skills: [skill-packages.md](docs/skill-packages.md),
  [openapi-wizard.md](docs/openapi-wizard.md),
  [skill-bundles.md](docs/skill-bundles.md),
  [skill-evaluation.md](docs/skill-evaluation.md).
- Running turns: [triggers.md](docs/triggers.md),
  [inhibitors.md](docs/inhibitors.md), [approvals.md](docs/approvals.md),
  [auto-approval.md](docs/auto-approval.md),
  [action-queue.md](docs/action-queue.md),
  [idempotency.md](docs/idempotency.md),
  [prompt-contributors.md](docs/prompt-contributors.md),
  [personalities.md](docs/personalities.md),
  [admin-agent.md](docs/admin-agent.md).
- Storage and search: [storage.md](docs/storage.md),
  [streaming-storage.md](docs/streaming-storage.md),
  [pdf-rendering.md](docs/pdf-rendering.md),
  [session-search.md](docs/session-search.md), [vision.md](docs/vision.md).
