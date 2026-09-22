#!/usr/bin/env bash
# Local development on a Mac: ollama on the host, the rest in the cluster.
#
#   scripts/dev-mac.sh            # checks ollama and the model, then skaffold dev -p mac
#   scripts/dev-mac.sh --small    # qwen3.5 instead, for a Mac with less memory
#   scripts/dev-mac.sh --tika     # with document extraction
#   scripts/dev-mac.sh -v info    # anything else is passed to skaffold
#
# Expects a cluster already running (Docker Desktop's Kubernetes, kind, or
# colima --kubernetes) and kubectl pointed at it. Starting one is left to you:
# which cluster is a choice, and a script that makes it quietly is a script
# that makes the wrong one.
set -euo pipefail

cd "$(dirname "$0")/.."

profile=mac
overlay=k8s/overlays/local-mac/kustomization.yaml
if [[ "${1:-}" == "--small" ]]; then
  # A smaller model, for a machine that cannot spare 22GB for the big one.
  profile=mac-small
  overlay=k8s/overlays/local-mac-small/kustomization.yaml
  shift
fi

if [[ "${1:-}" == "--tika" ]]; then
  # Document extraction. A combined overlay rather than a second -p flag,
  # because skaffold replaces the kustomize paths per profile rather than
  # merging them -- `-p mac -p tika` would take the tika overlay's base,
  # which has the Linux Docker bridge address instead of host.docker.internal.
  profile="${profile}-tika"
  shift
fi

# This clone's keys, if it has none yet. Silent when they already exist, and
# before anything slow so a fresh clone is not told about a missing file after
# an 18GB pull. The mac overlays build on ../local, so they read the same file.
scripts/dev-secrets.sh

ollama_url=http://localhost:11434

# Read out of the overlay rather than repeated here, so the model the script
# pulls is the model the cluster asks for.
model=$(awk '/name: OUTTURN_DEFAULT_MODEL/ { getline; gsub(/.*value: "|"$/, ""); print; exit }' "$overlay")
if [[ -z "$model" ]]; then
  echo "could not read OUTTURN_DEFAULT_MODEL from $overlay" >&2
  exit 1
fi

# Before anything slow: a pull is 18GB, and finding out afterwards that there
# was nowhere to deploy is an evening gone.
if ! kubectl cluster-info >/dev/null 2>&1; then
  echo "no cluster reachable from kubectl (context: $(kubectl config current-context 2>/dev/null || echo none))" >&2
  echo "start one in Docker Desktop, or: colima start --kubernetes" >&2
  exit 1
fi

if ! command -v ollama >/dev/null; then
  echo "ollama is not installed: brew install ollama" >&2
  exit 1
fi

if ! curl -sf "$ollama_url/api/version" >/dev/null; then
  echo "starting ollama"
  if command -v brew >/dev/null && brew list ollama >/dev/null 2>&1; then
    brew services start ollama
  else
    nohup ollama serve >"${TMPDIR:-/tmp}/ollama.log" 2>&1 &
  fi
  for _ in $(seq 30); do
    curl -sf "$ollama_url/api/version" >/dev/null && break
    sleep 1
  done
  if ! curl -sf "$ollama_url/api/version" >/dev/null; then
    echo "ollama did not come up on $ollama_url" >&2
    exit 1
  fi
fi

if ! ollama show "$model" >/dev/null 2>&1; then
  echo "pulling $model"
  ollama pull "$model"
fi

# Loaded now rather than on the first message, so the first reply is not also
# the one that waits for the weights.
echo "loading $model"
curl -sf "$ollama_url/api/generate" -d "{\"model\":\"$model\",\"keep_alive\":\"30m\"}" >/dev/null

# The flags AGENTS.md uses, so a rebuild can be asked for over the Control API
# with the same curl as on any other machine.
exec skaffold dev -p "$profile" \
  --auto-build=false --auto-deploy=false --auto-sync=false --rpc-http-port=50052 \
  "$@"
