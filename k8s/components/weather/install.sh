#!/bin/sh
# Installs the weather skill into the workspace, over the API.
#
# The same script as k8s/components/hollowbrook/install.sh with a different
# slug, host and skill directory -- copied rather than shared, which is the
# honest shape for something meant to be taken and adapted. Two copies will
# drift; a library a customer has to understand before writing their own is
# worse.
#
# What it demonstrates that Hollowbrook does not: a public host over https,
# which is the ordinary case. Hollowbrook needs the operator allowlist because
# it is inside the cluster; api.open-meteo.com needs nothing but a workspace
# rule, which this creates by declaring the host on the skill.
#
# Idempotent: a slug that already exists is left alone, so a restarted Job or a
# redeployed component does not write a second copy.
set -eu

api="${OUTTURN_API:-http://outturn-api:8080}"
slug=weather
host="${WEATHER_HOST:-api.open-meteo.com}"

say() { echo "install-weather: $*" >&2; }

# The API has to be up. Not a race worth losing to: a Job that starts with the
# deployment will usually get here first.
i=0
while ! curl -sf "$api/healthz" >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -lt 60 ] || { say "the API never became ready at $api"; exit 1; }
  sleep 2
done

# Signed in as the seeded admin. A real deployment would use a credential made
# for whatever does this -- skills:write and the authority to write an egress
# rule, not an administrator -- but the dev seed's admin is who exists here.
#
# Two calls, and a bearer token rather than the cookie jar. The session arrives
# as a Set-Cookie marked `Secure`, which browsers accept on localhost and curl
# does not store over plain http at all, so a cookie jar here stays empty and
# every call afterwards is a 401. The API expects this: "browsers send the
# HttpOnly session cookie; service-to-service callers send a bearer token"
# (src/api/router.rs). The token is the cookie's value, read out of the header.
#
# The first call asks which workspaces there are, because naming one is what
# makes the second call return a session rather than a list.
workspace="${OUTTURN_WORKSPACE_ID:-}"
if [ -z "$workspace" ]; then
  workspace=$(curl -sf "$api/v1/login" \
    -H 'content-type: application/json' \
    -d "{\"email\":\"${OUTTURN_ADMIN_EMAIL}\",\"password\":\"${OUTTURN_ADMIN_PASSWORD}\"}" \
    | jq -r '.workspaces[0].workspace_id // empty')
  [ -n "$workspace" ] || { say "signed in but there is no workspace to install into"; exit 1; }
fi

token=$(curl -sf -D - -o /dev/null "$api/v1/login" \
  -H 'content-type: application/json' \
  -d "{\"email\":\"${OUTTURN_ADMIN_EMAIL}\",\"password\":\"${OUTTURN_ADMIN_PASSWORD}\",\"workspace_id\":\"$workspace\"}" \
  | grep -i '^set-cookie: outturn_session=' | sed 's/^[^=]*=//; s/;.*//')
[ -n "$token" ] || { say "could not sign in"; exit 1; }
auth="authorization: Bearer $token"

# Already there: update it rather than stopping.
#
# Stopping was the first version of this, and it meant an edited skill never
# reached a cluster that had the old one -- the Job ran, said "already
# installed", and left the workspace with prose nobody had written for months.
# Whoever edits a skill and redeploys means for the edit to arrive.
#
# Prose changes by appending a version, never by overwriting one, so the
# history of what an agent was told stays readable. The files are written over
# in place, which is right for reference material: there is one current answer
# to how an operation is called.
existing=$(curl -sf "$api/v1/skills" -H "$auth" \
  | jq -r ".[] | select(.slug == \"$slug\") | .id" | head -1)

# The manifest is the skill body, and the detail is a file per operation --
# see docs/openapi-wizard.md. A body is composed into the prompt on every
# round of every turn, so an API written into one in full is paid for
# continuously; the agent reads an operation's file only when it needs it.
manifest=$(cat /skill/index.md)

if [ -n "$existing" ]; then
  id="$existing"
  # A version is only worth appending when the prose actually differs.
  # Otherwise every redeploy grows the history by one identical entry, and the
  # history stops being worth reading.
  current=$(curl -sf "$api/v1/skills/$id/versions" -H "$auth" | jq -r '.[0].body // ""')
  if [ "$current" = "$manifest" ]; then
    say "already installed, and unchanged"
  else
    curl -sf "$api/v1/skills/$id/versions" -H "$auth" \
      -H 'content-type: application/json' \
      -d "$(jq -n --arg body "$manifest" \
            '{body: $body, note: "installed by the component"}')" >/dev/null \
      || { say "could not update the skill"; exit 1; }
    say "updated skill $id"
  fi
else
  created=$(curl -sf "$api/v1/skills" -H "$auth" \
    -H 'content-type: application/json' \
    -d "$(jq -n --arg slug "$slug" --arg body "$manifest" --arg host "$host" \
          '{slug: $slug, name: "Weather",
            description: "Forecasts by coordinates.",
            body: $body, hosts: [$host]}')") \
    || { say "could not create the skill"; exit 1; }

  id=$(echo "$created" | jq -r .id)
  [ -n "$id" ] && [ "$id" != "null" ] || { say "created a skill with no id: $created"; exit 1; }
  say "created skill $id"
fi

# Declaring a host opens nothing. This is the second act: the workspace
# allowing its agents to ask for it, which is an egress rule tagged with the
# skill that wanted it.
curl -sf -X POST "$api/v1/skills/$id/hosts/approve" -H "$auth" >/dev/null \
  || { say "could not approve $host"; exit 1; }
say "approved $host"

# The detail files. Workspace scope, which is read-only to an agent -- the
# right default for reference material it consults and must never rewrite.
#
# Reached through a session because that is where the files endpoint lives,
# though the key a workspace-scoped path resolves to names no session. A
# throwaway one, named for what it is.
agent=$(curl -sf "$api/v1/agents" -H "$auth" | jq -r '.[0].id // empty')
[ -n "$agent" ] || { say "no agent to open a session with"; exit 1; }
session=$(curl -sf "$api/v1/agent-sessions" -H "$auth" \
  -H 'content-type: application/json' \
  -d "$(jq -n --arg a "$agent" '{agent_id: $a, title: "installing the Hollowbrook skill"}')" \
  | jq -r .id)
[ -n "$session" ] && [ "$session" != "null" ] || { say "could not open a session"; exit 1; }

for file in /skill/*.md; do
  name=$(basename "$file")
  curl -sf -X PUT "$api/v1/agent-sessions/$session/files/workspace/api/weather/$name" \
    -H "$auth" -H 'content-type: text/markdown' --data-binary "@$file" >/dev/null \
    || { say "could not upload $name"; exit 1; }
done
say "uploaded $(ls /skill/*.md | wc -l | tr -d ' ') files"

say "done"
