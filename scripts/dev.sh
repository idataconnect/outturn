#!/usr/bin/env bash
# Local development on Linux: everything in the cluster, the model server
# beside it.
#
#   scripts/dev.sh                          # skaffold dev with the Control API open
#   scripts/dev.sh --with tika              # plus document extraction
#   scripts/dev.sh --with hollowbrook,weather
#   scripts/dev.sh -v info                  # anything else is passed to skaffold
#
# `--with` takes any of k8s/components. Nothing is built or deployed until
# asked for: `scripts/build.sh` triggers a round over the Control API.
#
# Expects a cluster already running and kubectl pointed at it. Starting one is
# left to you: which cluster is a choice, and a script that makes it quietly is
# a script that makes the wrong one.
#
# On a Mac use scripts/dev-mac.sh instead -- ollama runs on the host there, and
# reaching it is the whole difference. Everything else is scripts/lib/dev.sh.
set -euo pipefail

cd "$(dirname "$0")/.."
source scripts/lib/dev.sh

dev_parse "$@"
dev_write_overlay local
dev_require_cluster "start one first, e.g.: kind create cluster"

# This clone's keys, if it has none yet. Before skaffold rather than in a
# build hook: the overlay reads the file through a secretGenerator, and
# kustomize renders before any deploy hook runs.
scripts/dev-secrets.sh

dev_skaffold
