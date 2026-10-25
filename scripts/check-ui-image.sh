#!/usr/bin/env bash
# Starts the UI image under the restrictions k8s/deploy gives its pod -- every
# capability dropped, no privilege escalation, a read-only root with /tmp the
# one writable place, user 65534 -- and checks it serves. v0.1.0 shipped an
# image that built, linted and tested clean and then could not exec its own
# server under that securityContext; nothing short of starting it would have
# said so.
#
#   scripts/check-ui-image.sh            # builds outturn-ui:check from ui/
#   scripts/check-ui-image.sh <image>    # checks an image already built
set -euo pipefail

cd "$(dirname "$0")/.."

image=${1:-}
if [[ -z $image ]]; then
  image=outturn-ui:check
  docker build -q -t "$image" ui >/dev/null
fi

name=outturn-ui-check-$$
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT

docker run -d --name "$name" \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --read-only --tmpfs /tmp \
  --user 65534:65534 \
  -e OUTTURN_BRAND_NAME="Check Desk" \
  -p 127.0.0.1::8080 \
  "$image" >/dev/null

# Waits on the server itself, until it answers or the container has exited.
# A container that exited at once has no port to ask about, so the port is
# looked up inside the wait rather than before it.
port=
for _ in $(seq 1 60); do
  if [[ $(docker inspect -f '{{.State.Running}}' "$name") != true ]]; then
    echo "the UI container exited under the pod's restrictions:" >&2
    docker logs "$name" >&2
    exit 1
  fi
  port=$(docker port "$name" 8080/tcp 2>/dev/null | head -1 | sed 's/.*://' || true)
  if curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

fail() {
  echo "$1" >&2
  docker logs "$name" >&2
  exit 1
}

curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null || fail "/healthz never answered"
curl -fsS "http://127.0.0.1:$port/" | grep -q '<div id="root"' || fail "/ is not the app"
# Written at startup, so this is also the check that the read-only root left it
# somewhere to write.
curl -fsS "http://127.0.0.1:$port/config.json" | grep -q '"Check Desk"' ||
  fail "/config.json does not carry OUTTURN_BRAND_NAME"

echo "the UI image starts and serves under the pod's restrictions"
