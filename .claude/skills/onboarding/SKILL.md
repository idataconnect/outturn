---
name: onboarding
description: Check that this machine can build and run outturn, and explain how to fix what cannot. Covers skaffold, kubectl, a reachable cluster, the container daemon, the image-registry heuristic that decides whether images are pushed to Docker Hub, ollama and its model, and this clone's generated keys. Use when setting up outturn on a new machine, when `skaffold dev` fails before any pod starts, when images are being pushed somewhere unexpected, or on request — "onboard me", "check my setup", "why won't this run".
---

# Onboarding

Diagnose, then explain. **Change nothing.**

Print the exact command that fixes each problem and let the person run it.
Several of these checks touch a cluster, and a cluster is not always one this
repository owns -- see *Someone else's cluster* below. Even the safe-looking
fixes are somebody's decision: which cluster, which package manager, whether a
33-day-old namespace beside this one is expendable.

Report every check, not only the failures. "It all passed" is the answer to
"why won't this run", and it is only believable if the reader can see what was
looked at.

## Reporting

One line per check, in this order, worst last so the fix is the last thing
read:

```
PASS  skaffold 2.24.0
PASS  kubectl reaches kind-kind
WARN  local-cluster is not pinned for kind-kind
FAIL  ollama is not reachable on :11434
```

Follow each WARN and FAIL with what it means and the command that fixes it.
Do not print a fix for a PASS.

## The checks

### 1. Tools

`skaffold version`, `kubectl version --client`, `docker version`, `kustomize
version`. Missing is a FAIL with the install command for whatever the machine
has -- `brew install` on a Mac, the distribution's packager on Linux.

`kustomize` is optional: skaffold has one built in, and a standalone binary is
only wanted for running `kustomize build` by hand. WARN, not FAIL.

### 2. A cluster

`kubectl config current-context` names one; `kubectl cluster-info` says
whether it answers. A context that exists but does not answer is the more
common failure -- a stopped Docker Desktop, a colima VM that was never
started -- and the message should say which of the two it is.

Do **not** create a cluster. Which cluster receives this is a decision, and
`scripts/dev-mac.sh` already refuses to make it, saying so in the same words.

### 3. Whether images get pushed to Docker Hub

The trap worth this whole skill.

Skaffold decides whether to push built images by guessing from the context
name: `kind-*`, `minikube`, `docker-desktop` and a few others are treated as
local, and everything else is assumed remote. A local cluster under a name it
does not recognise -- `dev`, `k3s-default`, a renamed kind cluster -- means
skaffold tries to **push four images to Docker Hub**, which fails slowly if
the person is not logged in and succeeds embarrassingly if they are.

Check what is pinned:

```bash
skaffold config list --all
```

A `kube-context` entry with `local-cluster: true` is pinned and correct. An
entry with no `local-cluster` key is relying on the heuristic, which is a WARN
even when the name happens to match -- it works today and breaks silently if
the cluster is ever renamed. The fix, with the real context name:

```bash
skaffold config set -k <context> local-cluster true
```

README.md gives this for colima, which skaffold does not recognise. It applies
to any local cluster not named to suit the guess.

### 4. The container daemon

`docker version` reaching a daemon. On a Mac this is Docker Desktop or
colima; under colima, `skaffold` also needs `DOCKER_HOST` pointing at its
socket, which `colima start` sets in the shell that ran it and nowhere else.

A daemon that answers but is not the one backing the cluster builds images
the cluster cannot see. Pods then sit in `ErrImagePull` with images that
exist locally, which reads as a registry problem and is not one.

### 5. ollama and the model

The gateway reaches ollama over the OpenAI protocol; nothing else needs it,
so a cluster without it comes up healthy and fails on the first message.

```bash
curl -sf http://localhost:11434/api/version
```

Which model should be there depends on the profile, and AGENTS.md is emphatic
about which: **qwen3.5** on Linux, **qwen3.8:27b-mlx** on a Mac. Read the
value out of the overlay rather than repeating it here -- `scripts/dev-mac.sh`
shows the awk that does it -- so this skill cannot drift from what the cluster
asks for.

Report the size before suggesting a pull. qwen3.8 is 18-23GB loaded and was
chosen on a 64GB M4; on a 32GB machine it is tight beside the cluster's VM,
and `scripts/dev-mac.sh --small` exists for that.

### 6. This clone's keys

`k8s/overlays/local/dev-secrets.env` holds the token keypair, the runtime key
and the seeded admin password, generated per clone and gitignored.

Its absence is **not** a failure: a pre-build hook on the first artifact
generates it, so `skaffold dev` handles it. It matters for anyone running
`kustomize build` or `skaffold render` directly, because those skip build
hooks and fail on the missing file with a message that names the path and not
the remedy. Report it as INFO with the fix:

```bash
scripts/dev-secrets.sh          # generate
scripts/dev-secrets.sh --print  # the admin password
```

### 7. Capacity

Worth a note, not a gate. Four images build from a Rust workspace and the
model wants most of a machine's memory. If `kubectl top nodes` answers, report
it; metrics-server is often absent on kind, and its absence is not a problem
to fix. Never fail a setup on this -- a tight machine still runs, and
`--small` is the answer when it does not.

## Someone else's cluster

Check what else is in the cluster before saying anything about deleting
things. `kubectl get ns` and `kubectl get pvc -A` take a second and have
already caught one case here: a 28Gi namespace from an unrelated project,
scaled to zero, sharing this kind cluster.

`skaffold delete` only removes what this repository declares, and a namespace
of somebody else's is not that. `kind delete cluster` takes everything.
Say which one is being proposed.

## After it passes

Point at what already exists rather than repeating it:

- `README.md` -- running locally, the keys, the UI on :3000
- `AGENTS.md` -- the Control API loop, why not to start your own
  `kubectl port-forward`, which models can call a tool, and why
  `reasoning_effort: "none"` makes a model look broken

The first command is `scripts/dev-mac.sh` on a Mac, or `skaffold dev` on
Linux.
