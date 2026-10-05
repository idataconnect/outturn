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

On a machine that has never run this, `.agents/skills/onboarding/SKILL.md`
lists requirements — the tools, a reachable cluster, the container
daemon, ollama (or compatible) and its model — and the one trap worth knowing
before it bites:
skaffold decides whether to push images by guessing from the kube-context
name, so a local cluster under an unfamiliar name means four images pushed to
Docker Hub. It diagnoses and explains; it changes nothing.

Start the cluster with the Control API open, so a build and deploy can be
triggered without hitting Enter:

```bash
scripts/dev.sh        # skaffold dev, Control API on :50052, nothing auto
                      # --with tika,hollowbrook adds components
scripts/build.sh      # in another terminal: one build-and-deploy round
```

Which is this, with the traps below already handled:

```bash
skaffold dev --auto-build=false --auto-deploy=false --auto-sync=false --rpc-http-port=50052
curl -X POST http://localhost:50052/v1/execute -d '{"build":true,"deploy":true}'
```

Build and deploy must go in **one** request; a lone deploy can softlock the
loop (skaffold #4886), which is why `build.sh` offers no way to ask for one.
`--trigger=manual` on its own does not work — it gates file watching, not the
API. Check `buildState.autoTrigger` in `/v1/state`: `true` means `/v1/execute`
returns `{}` and silently does nothing, which `build.sh` checks for rather than
leaving you to wonder why a build changed nothing.

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

Local dev seeds `admin@outturn.local`, with a password generated per clone
into `k8s/overlays/local/dev-secrets.env` — `scripts/dev-secrets.sh --print`
shows it. The keys live there too, gitignored and never committed; a build
generates them if they are missing.

## Models

Local development runs against ollama through the OpenAI protocol
(`OPENAI_BASE_URL`, no key). **Use qwen3.5.** It works well enough for most
tasks, including tool use. It fits in an 8Gi card when using 8-bit quantized
KV.

Setting thinking to off will sometimes cause strange behavior around tool
calling, such as increasing the number of pointless tool calls, and stopping
the turn right after a tool call without continuing.

On a Mac, `scripts/dev-mac.sh` builds on the `local-mac` overlay instead:
ollama on the host through `host.docker.internal`, and **qwen3.8:27b-mlx** as
the suggestion. Both scripts share `scripts/lib/dev.sh`, so `--with` and
everything else behave the same on either.

Which model, its context window and `context_budget` are this machine's
answers rather than the overlay's: asked on the first run, kept in the
gitignored `k8s/overlays/local/dev-machine.env`, and changed with
`scripts/dev-setup.sh --reconfigure`. With ollama the model is served as a tag
of its own, `outturn/<model>-ctx<window>`, carrying the window as `num_ctx`.
That tag is the only thing that sets the window whatever
`OLLAMA_CONTEXT_LENGTH` the server started with. See
[docs/local-development.md](docs/local-development.md).

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
  [session-search.md](docs/session-search.md), [vision.md](docs/vision.md).
