# outturn

*You define the process, outturn delivers the result.*

A multiworkspace agent platform.

## Running locally on a Mac

You need ollama (`brew install ollama`) and a local cluster. Docker Desktop's
own Kubernetes or kind will do, and so will colima, which is lighter:

```
colima start --kubernetes --cpu 8 --memory 16
# skaffold does not recognise colima as local, and would push the images it
# builds rather than leave them on the node
skaffold config set -k colima local-cluster true
```

Then:

```
scripts/dev-mac.sh
```

It starts ollama if it is not running, pulls `qwen3.8:27b-mlx` if it is
missing, loads it, and runs `skaffold dev -p mac`. That profile points the
gateway at ollama on the host through `host.docker.internal` and makes qwen3.8
the default model. Loaded, the model takes 18-23GB. It was chosen on a 64GB
M4; beside the cluster's VM, a 32GB machine will be tight.

Then start the UI with `cd ui && npm run dev`, open http://localhost:3000, and
sign in as `admin@outturn.local` / `outturn-dev`. To rebuild and redeploy
after a change:

```
curl -X POST http://localhost:50052/v1/execute -d '{"build":true,"deploy":true}'
```

On Linux, `skaffold dev` without a profile reaches ollama across the Docker
bridge and uses qwen3.5. [AGENTS.md](AGENTS.md) covers both in more depth,
including which models can be relied on to call a tool and why thinking is
left on.

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
