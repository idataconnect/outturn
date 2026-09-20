#!/usr/bin/env bash
# Local development on Linux: everything in the cluster, ollama beside it.
#
#   scripts/dev.sh              # skaffold dev with the Control API open
#   scripts/dev.sh -p tika      # anything else is passed to skaffold
#
# Nothing is built or deployed until asked for: `scripts/build.sh` triggers a
# round over the Control API, so a rebuild needs no keystroke in this terminal
# and can be asked for by something that is not a person.
#
# Expects a cluster already running and kubectl pointed at it. Starting one is
# left to you: which cluster is a choice, and a script that makes it quietly is
# a script that makes the wrong one.
#
# On a Mac use scripts/dev-mac.sh instead -- ollama runs on the host there, and
# reaching it is the whole difference.
set -euo pipefail

cd "$(dirname "$0")/.."

# Where the Control API listens. Shared with scripts/build.sh through the
# environment so the two cannot disagree about the port.
port="${OUTTURN_SKAFFOLD_RPC_PORT:-50052}"

# Before anything slow: finding out after a build that there was nowhere to
# deploy is an evening gone.
if ! kubectl cluster-info >/dev/null 2>&1; then
  echo "no cluster reachable from kubectl (context: $(kubectl config current-context 2>/dev/null || echo none))" >&2
  echo "start one first, e.g.: kind create cluster" >&2
  exit 1
fi

# The trap worth catching before it bites: skaffold decides whether to push by
# guessing from the kube-context name, and a local cluster under a name it does
# not recognise means four images pushed to a registry. Diagnosed and
# explained; nothing is changed, because which registry is right is not this
# script's call. See .agents/skills/onboarding/SKILL.md.
context=$(kubectl config current-context 2>/dev/null || echo "")
case "$context" in
  kind-* | docker-desktop | minikube | colima | k3d-*) ;;
  *)
    echo "warning: skaffold does not recognise the context '$context' as local," >&2
    echo "so it will PUSH images rather than loading them into the cluster." >&2
    echo "If that is not what you want, either rename the context or set:" >&2
    echo "  skaffold config set --kube-context '$context' local-cluster true" >&2
    echo >&2
    ;;
esac

# This clone's keys, if it has none yet. Silent when they already exist, and
# before skaffold rather than in a build hook: the overlay reads the file
# through a secretGenerator, and kustomize renders before any deploy hook runs.
scripts/dev-secrets.sh

echo "Control API on :$port -- trigger a build and deploy with scripts/build.sh"

# Auto-everything off so a file save does not start a build in the background
# while you are reading something. The Control API is what starts one instead.
exec skaffold dev \
  --auto-build=false --auto-deploy=false --auto-sync=false \
  --rpc-http-port="$port" \
  "$@"
