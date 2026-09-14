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
bridge and uses gemma4. [AGENTS.md](AGENTS.md) covers both in more depth.

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
