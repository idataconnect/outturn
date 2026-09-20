#!/usr/bin/env bash
# Asks a running `scripts/dev.sh` to build and deploy.
#
#   scripts/build.sh      # one build-and-deploy round
#
# Build and deploy go in ONE request, always. A lone deploy can softlock the
# dev loop (skaffold #4886), so this script does not offer a way to ask for
# one: the flag that would allow it is the bug.
set -euo pipefail

port="${OUTTURN_SKAFFOLD_RPC_PORT:-50052}"
api="http://localhost:$port"

if [[ $# -gt 0 ]]; then
  echo "unknown argument: $1" >&2
  echo "usage: scripts/build.sh" >&2
  exit 2
fi

if ! curl -sf "$api/v1/state" >/dev/null 2>&1; then
  echo "no skaffold Control API on $api" >&2
  echo "start one with: scripts/dev.sh" >&2
  exit 1
fi

# `--auto-build=false` is what makes /v1/execute work at all. With auto-trigger
# on, the call returns {} and silently does nothing -- the failure looks like a
# build that ran and changed nothing, which is a bad hour. Checked rather than
# assumed, because the flag lives in another terminal's command line.
if curl -sf "$api/v1/state" | grep -q '"autoTrigger":true'; then
  echo "skaffold has autoTrigger on, so /v1/execute would do nothing silently." >&2
  echo "Restart it with scripts/dev.sh, which passes --auto-build=false." >&2
  exit 1
fi

echo "building and deploying"
curl -sf -X POST "$api/v1/execute" -d '{"build":true,"deploy":true}' >/dev/null

# The request returns as soon as skaffold accepts it, not when the deploy is
# done. What it produced is in the dev.sh terminal, which is where the build
# log already goes.
echo "asked -- watch scripts/dev.sh for progress"
