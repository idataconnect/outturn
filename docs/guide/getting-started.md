# Getting started

From a clean machine to a local cluster with the UI signed in.

## What you need

Rust (stable, edition 2024), Node 22 or newer, `kubectl`, `skaffold`, a
container runtime and a local Kubernetes cluster. A model server too: ollama,
or anything else that speaks the OpenAI chat-completions protocol. Everything
else the build fetches itself.

The onboarding check in `.agents/skills/onboarding/SKILL.md` looks at a machine
and says what is missing. It changes nothing.

!!! warning "Name your cluster as local"
    skaffold decides whether to push the images it builds by guessing from the
    kube-context name. A local cluster under a name it does not recognize means
    four images pushed to Docker Hub. Tell it the cluster is local before the
    first build:

    ```bash
    skaffold config set -k <context> local-cluster true
    ```

## Start it

=== "Mac"

    Install ollama (`brew install ollama`) and start a cluster. Docker
    Desktop's Kubernetes or kind will do, and so will colima, which is lighter:

    ```bash
    colima start --kubernetes --cpu 8 --memory 16
    skaffold config set -k colima local-cluster true
    ```

    Then:

    ```bash
    scripts/dev-mac.sh
    ```

    It starts ollama if it is not running, and the gateway reaches it on the
    host through `host.docker.internal`.

=== "Linux"

    Start ollama yourself, then:

    ```bash
    scripts/dev.sh
    ```

    The gateway reaches ollama across the Docker bridge.

The first run asks which model, at what context window, and how much of a
conversation to send it, and keeps the answers in
`k8s/overlays/local/dev-machine.env`. The suggestion is qwen3.5, or
qwen3.8:27b-mlx on a Mac with enough memory. What each answer drives, and how
to change one, is in [This machine's model](../local-development.md).

`--with` adds optional components from `k8s/components`, such as document
extraction or the Hollowbrook guesthouse:

```bash
scripts/dev.sh --with tika,hollowbrook
```

## Sign in

Start the UI outside the cluster:

```bash
cd ui && npm run dev
```

Open <http://localhost:3000> and sign in as `admin@outturn.local`. The password
is this clone's own, generated with the cluster's keys:

```bash
scripts/dev-secrets.sh --print
```

## After a change

The dev script leaves skaffold waiting rather than rebuilding on every save.
To build and deploy once:

```bash
scripts/build.sh
```

skaffold forwards the API on 18080, the gateway on 18081, the runtime on 18082
and Postgres on 15432, and keeps them alive across redeploys. Do not start your
own `kubectl port-forward` beside it: it will not reconnect, and it pushes
skaffold onto other ports without saying so.

## Keys

The local cluster's keys are generated per clone and never committed. They
live in `k8s/overlays/local/dev-secrets.env`, which is gitignored:

```bash
scripts/dev-secrets.sh           # generate if absent; the dev scripts do this for you
scripts/dev-secrets.sh --force   # rotate, signing out existing sessions
```

For a real deployment, `scripts/deploy-prod.sh` generates a token keypair and a
Secret, asks for what it cannot know, and refuses to proceed if the dev seed is
enabled. It writes manifests and stops; applying them is left to you.

## Tests

```bash
cargo test
```

runs what is fast and needs nothing. The suites that need Postgres and a
gateway are behind a flag, and run against the ports skaffold forwards:

```bash
TEST_DATABASE_URL='postgres://outturn:outturn-dev@localhost:15432/outturn_test' \
GATEWAY_URL=http://localhost:18081 \
cargo test --features integration-tests
```

Nothing in either calls a paid model. The one suite that does needs
`--features live-providers`, typed deliberately, and nothing else implies it.

## Next

[Concepts](concepts.md) explains the tiers and the words used everywhere else,
and [Take it for a spin](../take-it-for-a-spin.md) builds an agent that books
rooms.
