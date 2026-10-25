# outturn

A fast and secure foundation for companies that want to build and run their
own agent platform.

Early, and under active development: interfaces may change. Built and
supported by [i Data Connect!](https://www.idataconnect.com/products/outturn)

<!-- The walkthrough: scripts/demo-video.sh records it, and GitHub plays it
     from an upload's user-attachments URL on the line below. -->
https://github.com/user-attachments/assets/6536e4ad-234b-469b-abcd-19c1365ceb6b

Workspaces deploy agents that serve their own customers, with isolation, usage
attribution and security boundaries built in rather than added later. The
runtime runs agent code in a WebAssembly sandbox with deny-by-default egress;
the gateway holds the credentials and never shares them with the sandbox; the
API holds the signing key and the database. Each tier holds only what it needs
and nothing more.

Rust, Axum, Tokio, PostgreSQL, WASM, Kubernetes. Apache-2.0.

Documentation: <https://idataconnect.github.io/outturn/>, built from
[docs/](docs/) with MkDocs (`mkdocs serve` after
`pip install -r requirements-docs.txt`).

## Who this is for

A company whose customers each need agents of their own, and who would
otherwise build the tenancy, the sandbox and the security boundaries before
writing a single feature.

A workspace is whatever a tenant is in your product:

- **A finance application** where each workspace is a customer, and their
  agents reconcile transactions against the books they already keep with you.
- **Freight brokerage software** where each workspace is a brokerage, and
  their agents quote loads and chase carriers on behalf of the shippers they
  serve — so one deployment carries many brokerages, each with many customers
  of their own.
- **A rental marketplace** where each workspace is an owner, and their agents
  answer guests and manage a calendar.

The shape is the same in all three: you run one platform, your customers get
agents that reach their own systems, and nobody's agent can see anybody
else's anything.

That last part is what takes the time to build, and it is done. Isolation,
per-tenant usage attribution, a WebAssembly sandbox that holds no credentials,
deny-by-default egress with per-workspace rules, signed per-turn tokens, a
circuit breaker that does not let one tenant's failures trip another's. The
work left is the part that is yours: what your agents know, and what they can
reach.

## Try it

Everything runs in a local Kubernetes cluster. The model can be a hosted one,
with an API key you already have, or a local one, which is free but needs a
capable machine.

### Prerequisites

A container runtime, a local cluster (kind, Docker Desktop's own Kubernetes,
or colima), `kubectl`, `skaffold`, Node 22 or newer, and Rust (stable,
edition 2024). Everything else the build needs it fetches itself.

On a Mac, colima is the lightest cluster:

```
colima start --kubernetes --cpu 8 --memory 16
# skaffold does not recognize colima as local, and would push the images it
# builds rather than leave them on the node
skaffold config set -k colima local-cluster true
```

### With an API key (quickest)

No GPU and no model download. You need a key from Anthropic, OpenAI or Gemini.

```
scripts/dev.sh            # on a Mac: scripts/dev-mac.sh
```

The first run asks whether the model is hosted or local; answer `hosted`. In
another terminal, `scripts/build.sh` builds and deploys, and
`cd ui && npm run dev` starts the UI. Open http://localhost:3000 and sign in
as `admin@outturn.local`, with the password `scripts/dev-secrets.sh --print`
shows.

Then go to Platform → Models and paste the key. The setup there offers
recipes, each a chain of models with a fallback, and makes a test call when
it is done. A provider without a recipe is an account, a model and a chain
added by hand on the same page. The key is sealed in the browser to the
gateway, so it is never written to a file, the environment or a manifest.

### Free and local

A local model through ollama, or llama.cpp, on this machine or another on
the network. Answer `local` to the first question, then which model, at what
context window, and how much of a conversation to send it. The suggestions
come from what can be seen of the machine.

- **On a Mac**, `scripts/dev-mac.sh` needs ollama (`brew install ollama`),
  starts it if it is not running, pulls the model and loads it at the window
  you chose. The suggestion is qwen3.8:27b-mlx, 18-23GB loaded, which wants a
  64GB machine; below 48GB it is qwen3.5 at half the window. The gateway
  reaches ollama on the host through `host.docker.internal`.
- **On Linux**, `scripts/dev.sh` expects ollama already running, listening
  beyond loopback, and reaches it across the Docker bridge. The suggestion is
  qwen3.5, which fits an 8GB card with ollama's 8-bit KV cache.

The cluster comes up with a chain to that model already made, so there is
nothing to configure on the Models page.
[docs/local-development.md](docs/local-development.md) has what each answer
drives, which models can be relied on to call a tool, and why thinking is
left on. [docs/take-it-for-a-spin.md](docs/take-it-for-a-spin.md) is half an
hour from a cold Mac to an agent booking rooms at a guesthouse you are also
running.

### Either way

Two flags, and anything else goes to skaffold:

```
scripts/dev.sh --with tika          # plus document extraction
scripts/dev.sh --with tika,petstore
scripts/dev.sh --reconfigure        # ask the model questions again
```

`--with` takes anything in `k8s/components`, and `scripts/dev-mac.sh` takes
it too -- both scripts are thin wrappers over `scripts/lib/dev.sh`. They are
kustomize components, so they compose: the script writes an overlay listing
whichever were asked for, and adding a third component needs no change to the
script. The overlay is generated rather than committed because the
alternative is one per combination -- and two features already make four.

`scripts/build.sh` is one build-and-deploy round after a change.

## Keys

The local cluster's keys are generated per clone, never committed:

```
scripts/dev-secrets.sh           # generate if absent; the dev scripts do this for you
scripts/dev-secrets.sh --print   # what the seeded admin signs in with
scripts/dev-secrets.sh --force   # rotate, signing out existing sessions
```

They land in `k8s/overlays/local/dev-secrets.env`, which is gitignored and
read by the overlay's `secretGenerator`. Delete it and run the script again to
start over. If a build stops with `no such file or directory` for that path,
it has not been generated yet.

For a real deployment, `scripts/deploy-prod.sh` generates a token keypair and
a Secret, asks for the things it cannot know, and refuses to proceed if the
dev seed is enabled, if a key this repository once published appears, or if
the signing keypair does not agree. It writes manifests and stops; applying
them is left to you.

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

wit-bindgen rust wit/ --world agent-world --out-dir agents/default/src --runtime-path wit_bindgen_rt --format
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

`cargo test` runs only what needs nothing external. That includes the agent
component tests, which run against a scripted in-process gateway rather than a
live model -- fast, deterministic, and able to arrange failures a real provider
makes awkward, such as a 503 or a stream truncated mid-generation.

The suites that need running services are gated behind a feature flag, so a
bare checkout builds and runs the fast tests without silently skipping the
rest. They are separated by what they require rather than by how they are
written:

```
# Postgres and a gateway, with skaffold dev providing the port-forwards.
# Note the *_test database: these create and drop schemas, and refuse to run
# against a database whose name does not contain "test".
TEST_DATABASE_URL=postgres://outturn:outturn-dev@localhost:15432/outturn_test \
GATEWAY_URL=http://localhost:18081 \
  cargo test --features integration-tests
```

Each database test runs in its own Postgres schema, created on setup and
dropped on success, so they are isolated from each other and run in parallel.
A failing test leaves its schema behind to inspect.

A missing environment variable fails rather than skipping. A skipped test that
reports success would let a misconfigured CI run green having tested nothing.

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) covers the test tiers, the conventions, and
what to open an issue about first. Security reports go through
[SECURITY.md](SECURITY.md), privately, rather than the issue tracker -- it also
explains why secret scanners flag this repository's early history, and why that
is expected.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
