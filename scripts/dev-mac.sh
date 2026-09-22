#!/usr/bin/env bash
# Local development on a Mac: ollama on the host, the rest in the cluster.
#
#   scripts/dev-mac.sh                    # checks ollama and the model, then skaffold dev
#   scripts/dev-mac.sh --small            # qwen3.5 instead, for a Mac with less memory
#   scripts/dev-mac.sh --with tika        # plus document extraction
#   scripts/dev-mac.sh --with tika,petstore
#   scripts/dev-mac.sh -v info            # anything else is passed to skaffold
#
# `--with` takes any of k8s/components. They are kustomize components, which
# compose: the overlay this script generates lists whichever were asked for,
# and adding a third needs nothing here.
#
# Generated rather than committed because the alternative is one overlay per
# combination -- local-mac-tika, local-mac-petstore,
# local-mac-tika-petstore, and the same again under --small. Two features
# already made four, and nobody would keep them in step.
#
# Expects a cluster already running (Docker Desktop's Kubernetes, kind, or
# colima --kubernetes) and kubectl pointed at it. Starting one is left to you:
# which cluster is a choice, and a script that makes it quietly is a script
# that makes the wrong one.
set -euo pipefail

cd "$(dirname "$0")/.."

base=local-mac
if [[ "${1:-}" == "--small" ]]; then
  # A smaller model, for a machine that cannot spare 22GB for the big one.
  base=local-mac-small
  shift
fi
overlay="k8s/overlays/$base/kustomization.yaml"

features=()
if [[ "${1:-}" == "--with" ]]; then
  if [[ -z "${2:-}" ]]; then
    echo "--with needs a component: $(ls k8s/components | tr '\n' ' ')" >&2
    exit 2
  fi
  IFS=, read -r -a features <<<"$2"
  shift 2
fi

# Named rather than assumed: a typo here would otherwise produce an overlay
# kustomize refuses, and the error it gives names a generated path nobody
# wrote.
for feature in ${features[@]+"${features[@]}"}; do
  if [[ ! -d "k8s/components/$feature" ]]; then
    echo "no such component: $feature" >&2
    echo "available: $(ls k8s/components | tr '\n' ' ')" >&2
    exit 2
  fi
done

# A second -p cannot work: this script passes its own, skaffold takes the last
# rather than merging them, and the cluster comes up on whatever base that
# profile names -- for the tika profile, the Linux Docker bridge instead of
# host.docker.internal. It then fails one turn at a time in the gateway's log,
# long after the thing that caused it, which is why it is refused here.
for arg in "$@"; do
  case "$arg" in
    -p | --profile | -p=* | --profile=*)
      echo "this script passes its own profile; a second -p replaces it." >&2
      echo "for extra services use: scripts/dev-mac.sh --with tika" >&2
      exit 2
      ;;
  esac
done

# The overlay this run deploys: the base, plus whichever components were
# asked for. Written under the repository rather than in /tmp because
# kustomize resolves `resources` and `components` relative to the file, and a
# path out of the tree cannot reach back into it.
generated=k8s/overlays/.generated
mkdir -p "$generated"
{
  echo "# Written by scripts/dev-mac.sh. Not committed, and safe to delete:"
  echo "# every run replaces it. Edit the base or the component instead."
  echo "apiVersion: kustomize.config.k8s.io/v1beta1"
  echo "kind: Kustomization"
  echo
  echo "resources:"
  echo "  - ../$base"
  if [[ ${#features[@]} -gt 0 ]]; then
    echo
    echo "components:"
    for feature in "${features[@]}"; do
      echo "  - ../../components/$feature"
    done
  fi
} >"$generated/kustomization.yaml"

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
exec skaffold dev -p generated \
  --auto-build=false --auto-deploy=false --auto-sync=false --rpc-http-port=50052 \
  "$@"
