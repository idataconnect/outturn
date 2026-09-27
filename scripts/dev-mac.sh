#!/usr/bin/env bash
# Local development on a Mac: ollama on the host, the rest in the cluster.
#
#   scripts/dev-mac.sh                    # checks ollama and the model, then skaffold dev
#   scripts/dev-mac.sh --with tika        # plus document extraction
#   scripts/dev-mac.sh --with tika,petstore
#   scripts/dev-mac.sh --reconfigure      # ask the model questions again
#   scripts/dev-mac.sh -v info            # anything else is passed to skaffold
#
# `--with` takes any of k8s/components. What differs from Linux is the base
# overlay and that this script starts ollama; the rest is scripts/lib/dev.sh.
#
# The first run asks which model, at what context window, and how much of a
# conversation to send it, suggesting the smaller model on a Mac with less
# than 48GB -- see scripts/dev-setup.sh and docs/local-development.md.
#
# Expects a cluster already running (Docker Desktop's Kubernetes, kind, or
# colima --kubernetes) and kubectl pointed at it. Starting one is left to you:
# which cluster is a choice, and a script that makes it quietly is a script
# that makes the wrong one.
set -euo pipefail

cd "$(dirname "$0")/.."
source scripts/lib/dev.sh

for arg in "$@"; do
  # Was an overlay naming a smaller model and window. Both are answers now,
  # and a flag that silently did nothing would deploy the big model on the
  # machine it was meant to spare.
  if [[ "$arg" == "--small" ]]; then
    echo "--small is now an answer rather than a flag. Choose qwen3.5 at 16384 tokens with:" >&2
    echo "  scripts/dev-setup.sh --reconfigure" >&2
    exit 2
  fi
done

dev_parse "$@"

# Questions first, and before anything slow, so a fresh clone is not asked
# about its model after an 18GB pull.
dev_machine_setup "$dev_reconfigure"
dev_write_overlay local-mac

# This clone's keys, if it has none yet. Silent when they already exist. The
# mac overlay builds on ../local, so it reads the same file.
scripts/dev-secrets.sh

# Before the pull: 18GB, and finding out afterwards that there was nowhere to
# deploy is an evening gone.
dev_require_cluster "start one in Docker Desktop, or: colima start --kubernetes"

# Called by dev_ollama_prepare when nothing answers. Here rather than in the
# library because on a Mac ollama is an app this script can start; on Linux
# it is a service somebody else runs.
dev_ollama_start() {
  local url=$1
  case "$url" in
    http://localhost:* | http://127.0.0.1:*) ;;
    *) return 0 ;;
  esac
  if ! command -v ollama >/dev/null; then
    echo "ollama is not installed: brew install ollama" >&2
    exit 1
  fi
  echo "starting ollama"
  if command -v brew >/dev/null && brew list ollama >/dev/null 2>&1; then
    brew services start ollama
  else
    nohup ollama serve >"${TMPDIR:-/tmp}/ollama.log" 2>&1 &
  fi
  for _ in $(seq 30); do
    curl -sf "$url/api/version" >/dev/null && break
    sleep 1
  done
}

dev_ollama_prepare
dev_clear_stale_settings_job
dev_skaffold
