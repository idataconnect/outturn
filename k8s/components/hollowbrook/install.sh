#!/bin/sh
# Installs the Hollowbrook skill into the workspace, over the API.
#
# This is the shape a customer's own integration takes. Nothing here is
# privileged and nothing is patched into outturn: it signs in, calls the
# endpoints the platform already publishes, and could be a script on somebody's
# laptop instead of a Job. An integration that needed a Rust module in the
# platform would need a fork, and a fork fights every upgrade -- which is the
# argument ui/src/themes/README.md already makes about skinning, and it holds
# here for the same reason.
#
# Idempotent: a slug that already exists is left alone, so a restarted Job or a
# redeployed component does not write a second copy.
set -eu

api="${OUTTURN_API:-http://outturn-api:8080}"
slug=hollowbrook
host="${HOLLOWBROOK_HOST:-outturn-hollowbrook:8084}"

say() { echo "install-hollowbrook: $*" >&2; }

# The API has to be up. Not a race worth losing to: a Job that starts with the
# deployment will usually get here first.
i=0
while ! curl -sf "$api/healthz" >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -lt 60 ] || { say "the API never became ready at $api"; exit 1; }
  sleep 2
done

# Signed in as the seeded admin. A real deployment would use a credential made
# for whatever does this, with skills:write and egress rules -- not the
# administrator -- but the dev seed's admin is who exists here.
login=$(curl -sf "$api/v1/login" \
  -H 'content-type: application/json' \
  -d "{\"email\":\"${OUTTURN_ADMIN_EMAIL}\",\"password\":\"${OUTTURN_ADMIN_PASSWORD}\",\"workspace_id\":\"${OUTTURN_WORKSPACE_ID:-}\"}" \
  -c /tmp/cookies -w '\n%{http_code}') || { say "could not sign in"; exit 1; }

code=$(echo "$login" | tail -1)
[ "$code" = "200" ] || { say "sign-in answered $code"; exit 1; }

body=$(echo "$login" | sed '$d')
status=$(echo "$body" | sed -n 's/.*"status":"\([^"]*\)".*/\1/p')
if [ "$status" = "select_workspace" ]; then
  # More than one workspace and none named: take the first, which in a seeded
  # cluster is the one the seed made.
  ws=$(echo "$body" | sed -n 's/.*"workspaces":\[{"workspace_id":"\([^"]*\)".*/\1/p')
  [ -n "$ws" ] || { say "signed in but no workspace to choose"; exit 1; }
  curl -sf "$api/v1/session/workspace" -b /tmp/cookies -c /tmp/cookies \
    -H 'content-type: application/json' -d "{\"workspace_id\":\"$ws\"}" >/dev/null \
    || { say "could not select a workspace"; exit 1; }
fi

# Already there: nothing to do, and nothing to overwrite.
if curl -sf "$api/v1/skills" -b /tmp/cookies | grep -q "\"slug\":\"$slug\""; then
  say "already installed"
  exit 0
fi

# The manifest is the skill body, and the detail is a file per operation --
# see docs/openapi-wizard.md. A body is composed into the prompt on every
# round of every turn, so an API written into one in full is paid for
# continuously; the agent reads an operation's file only when it needs it.
manifest=$(cat /skill/index.md)
created=$(curl -sf "$api/v1/skills" -b /tmp/cookies \
  -H 'content-type: application/json' \
  -d "$(jq -n --arg slug "$slug" --arg body "$manifest" --arg host "$host" \
        '{slug: $slug, name: "Hollowbrook House",
          description: "Rooms and bookings for the guesthouse.",
          body: $body, hosts: [$host]}')") \
  || { say "could not create the skill"; exit 1; }

id=$(echo "$created" | jq -r .id)
[ -n "$id" ] && [ "$id" != "null" ] || { say "created a skill with no id: $created"; exit 1; }
say "created skill $id"

# Declaring a host opens nothing. This is the second act: the workspace
# allowing its agents to ask for it, which is an egress rule tagged with the
# skill that wanted it.
curl -sf -X POST "$api/v1/skills/$id/hosts/approve" -b /tmp/cookies >/dev/null \
  || { say "could not approve $host"; exit 1; }
say "approved $host"

# The detail files. Workspace scope, which is read-only to an agent -- the
# right default for reference material it consults and must never rewrite.
#
# Reached through a session because that is where the files endpoint lives,
# though the key a workspace-scoped path resolves to names no session. A
# throwaway one, named for what it is.
agent=$(curl -sf "$api/v1/agents" -b /tmp/cookies | jq -r '.[0].id // empty')
[ -n "$agent" ] || { say "no agent to open a session with"; exit 1; }
session=$(curl -sf "$api/v1/agent-sessions" -b /tmp/cookies \
  -H 'content-type: application/json' \
  -d "$(jq -n --arg a "$agent" '{agent_id: $a, title: "installing the Hollowbrook skill"}')" \
  | jq -r .id)
[ -n "$session" ] && [ "$session" != "null" ] || { say "could not open a session"; exit 1; }

for file in /skill/*.md; do
  name=$(basename "$file")
  curl -sf -X PUT "$api/v1/agent-sessions/$session/files/workspace/api/hollowbrook/$name" \
    -b /tmp/cookies -H 'content-type: text/markdown' --data-binary "@$file" >/dev/null \
    || { say "could not upload $name"; exit 1; }
done
say "uploaded $(ls /skill/*.md | wc -l | tr -d ' ') files"

say "done"
