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

# A request whose body is wanted, failing with what the API said rather than
# with curl's exit code. `-f` alone hides the reason, which is how a refused
# host once read as "could not create the skill" and nothing more.
call() {
  resp=$(curl -s -w '\n%{http_code}' "$@") || { say "could not reach $api"; return 1; }
  code=$(printf '%s' "$resp" | tail -n 1)
  body=$(printf '%s' "$resp" | sed '$d')
  case "$code" in
    2*) printf '%s' "$body" ;;
    *) say "$code from the API: $body"; return 1 ;;
  esac
}

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

# The manifest is the skill body and the rest are its files, published
# together as one version -- see docs/skill-bundles.md. A body is composed into
# the prompt on every round of every turn, so an API written into one in full
# is paid for continuously; the agent reads an operation's file, as
# skill/hollowbrook/<operation>.md, only when it needs it.
dir="${SKILL_DIR:-/skill}"
manifest=$(cat "$dir/index.md")
files=$(for f in "$dir"/*.md; do
  name=$(basename "$f")
  [ "$name" = index.md ] && continue
  jq -n --arg path "$name" --rawfile content "$f" '{path: $path, content: $content}'
done | jq -s .)

existing=$(curl -sf "$api/v1/skills" -H "$auth" \
  | jq -r ".[] | select(.slug == \"$slug\") | .id" | head -1)

# Already there: publish what this component now says, rather than stopping.
# Stopping was the first version of this, and it meant an edited skill never
# reached a cluster that had the old one. The API appends a version only when
# something differs, so a redeploy grows no history.
if [ -n "$existing" ]; then
  id="$existing"
  published=$(call "$api/v1/skills/$id/versions" -H "$auth" \
    -H 'content-type: application/json' \
    -d "$(jq -n --arg body "$manifest" --arg host "$host" --argjson files "$files" \
          '{body: $body, hosts: [$host], files: $files, note: "installed by the component"}')") \
    || { say "could not update the skill"; exit 1; }
  # The same version number as before means nothing changed: the API appends
  # only when something differs.
  say "skill $id is at v$(echo "$published" | jq -r .ordinal)"
else
  created=$(call "$api/v1/skills" -H "$auth" \
    -H 'content-type: application/json' \
    -d "$(jq -n --arg slug "$slug" --arg body "$manifest" --arg host "$host" --argjson files "$files" \
          '{slug: $slug, name: "Hollowbrook House",
            description: "Rooms and bookings for the guesthouse.",
            body: $body, hosts: [$host], files: $files}')") \
    || { say "could not create the skill"; exit 1; }

  id=$(echo "$created" | jq -r .id)
  [ -n "$id" ] && [ "$id" != "null" ] || { say "created a skill with no id: $created"; exit 1; }
  say "created skill $id"
fi

# Declaring a host opens nothing. This is the second act: the workspace
# allowing its agents to ask for it, which is an egress rule tagged with the
# skill that wanted it.
call -X POST "$api/v1/skills/$id/hosts/approve" -H "$auth" >/dev/null \
  || { say "could not approve $host"; exit 1; }
say "approved $host"

say "done"
