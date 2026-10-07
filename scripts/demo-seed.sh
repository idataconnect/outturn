#!/usr/bin/env bash
# Fills an empty local workspace with something worth looking at: the
# Hollowbrook skill and its Front desk agent, a second version of the skill so
# its history has a difference to show, a skill whose host nobody has approved
# yet, settings at each level of the cascade, and a few real conversations --
# the last of them held for a person's approval.
#
# For screenshots and the README video, against the cluster from
# scripts/dev.sh with --with hollowbrook. It runs real turns on the local
# model, so it takes a few minutes. Start from an empty database: it adds,
# and a second run adds a second set of conversations.
#
#     scripts/demo-seed.sh
set -euo pipefail

cd "$(dirname "$0")/.."
api="${OUTTURN_API:-http://localhost:18080}"
eval "$(scripts/dev-secrets.sh --print 2>/dev/null | grep -E '^OUTTURN_DEV_ADMIN_(EMAIL|PASSWORD)=')"

say() { echo "demo-seed: $*" >&2; }

# The Hollowbrook installer does the skill, its host and the Front desk agent,
# and is idempotent, so it goes first and this script builds on what it made.
OUTTURN_API="$api" OUTTURN_ADMIN_EMAIL="$OUTTURN_DEV_ADMIN_EMAIL" \
  OUTTURN_ADMIN_PASSWORD="$OUTTURN_DEV_ADMIN_PASSWORD" \
  SKILL_DIR=k8s/components/hollowbrook/skill \
  PROMPT_FILE=k8s/components/hollowbrook/system-prompt.txt \
  sh k8s/components/hollowbrook/install.sh

# Signed in the way the installer explains: a bearer token read out of the
# session cookie, which curl will not keep over plain http.
login='{"email":"'"$OUTTURN_DEV_ADMIN_EMAIL"'","password":"'"$OUTTURN_DEV_ADMIN_PASSWORD"'"'
workspace=$(curl -sf "$api/v1/login" -H 'content-type: application/json' -d "$login}" \
  | jq -r '.workspaces[0].workspace_id')
token=$(curl -sf -D - -o /dev/null "$api/v1/login" -H 'content-type: application/json' \
  -d "$login,\"workspace_id\":\"$workspace\"}" \
  | grep -i '^set-cookie: outturn_session=' | sed 's/^[^=]*=//; s/;.*//')

call() {
  local method=$1 path=$2 body=${3:-}
  curl -sf -X "$method" "$api$path" -H "authorization: Bearer $token" \
    -H 'content-type: application/json' ${body:+-d "$body"}
}

skill_id() { call GET '/v1/skills?limit=200' | jq -r ".items[] | select(.slug == \"$1\") | .id"; }
agent_id() { call GET '/v1/agents?limit=200' | jq -r ".items[] | select(.slug == \"$1\") | .id"; }

# A second version of the Hollowbrook skill, so its history has two to compare.
# The same files with one rule added to the manifest: the diff a reader should
# see is a change of behavior, not a reformatting.
hollowbrook=$(skill_id hollowbrook)
dir=k8s/components/hollowbrook/skill
files=$(for f in "$dir"/*.md; do
  [ "$(basename "$f")" = index.md ] && continue
  jq -n --arg path "$(basename "$f")" --rawfile content "$f" '{path: $path, content: $content}'
done | jq -s .)
body="$(cat "$dir/index.md")

Read a booking back to the guest -- name, room and dates -- before you make it."
v=$(call POST "/v1/skills/$hollowbrook/versions" "$(jq -n --arg body "$body" --argjson files "$files" \
  '{body: $body, hosts: ["outturn-hollowbrook:8084"], files: $files,
    note: "Confirm the guest, room and dates before booking"}')" | jq -r .ordinal)
say "hollowbrook is at v$v"

# A skill that declares a host and is left unapproved: declaring one opens
# nothing, and the skill's page is where a workspace admin decides.
if [ -z "$(skill_id local-weather)" ]; then
  call POST /v1/skills "$(jq -n '{
    slug: "local-weather", name: "Local weather",
    description: "The forecast near the house, for guests planning their day.",
    hosts: ["api.open-meteo.com"],
    body: "When a guest asks about the weather, fetch\nhttps://api.open-meteo.com/v1/forecast?latitude=-34.66&longitude=150.85&daily=temperature_2m_max,precipitation_probability_max&timezone=auto\nand answer in a sentence: the high and the chance of rain, today and tomorrow."
  }')" >/dev/null
  say "created local-weather, its host unapproved"
fi

# A second agent, so the agent list is not a list of one. Given Hollowbrook
# only: a skill cannot be bound while a host it declares is unapproved, so
# the weather is the Concierge's once somebody approves it.
if [ -z "$(agent_id concierge)" ]; then
  concierge=$(call POST /v1/agents "$(jq -n '{
    slug: "concierge", name: "Concierge",
    description: "Suggestions and local knowledge for guests during their stay.",
    system_prompt: "You help guests of Hollowbrook House, a guesthouse, enjoy their stay: the weather, what is nearby, and their booking. Be warm and brief."
  }')" | jq -r .id)
  say "created the Concierge"
else
  concierge=$(agent_id concierge)
fi
call PUT "/v1/agents/$concierge/skills" "$(jq -n --arg a "$hollowbrook" '[{skill_id: $a}]')" >/dev/null

# One setting at each level, so the cascade has something to show: the
# operator's default, the workspace's choice over it, and an agent's over that.
call PUT /v1/platform/settings/temperature '{"value": 0.7}' >/dev/null
call PUT /v1/settings/temperature '{"value": 0.4}' >/dev/null
call PUT "/v1/agents/$concierge/settings/temperature" '{"value": 0.9}' >/dev/null
call PUT /v1/platform/settings/context_budget '{"value": 60000}' >/dev/null
say "set temperature at each level"

# A turn, waited out. Done when the prompt's job leaves the queue, which
# includes being held for an approval. Leaves the job's state in $state.
ask() {
  local session=$1 content=$2
  call POST "/v1/agent-sessions/$session/messages" "$(jq -n --arg c "$content" '{content: $c}')" >/dev/null
  for _ in $(seq 1 120); do
    sleep 5
    state=$(call GET "/v1/agent-sessions/$session/messages" \
      | jq -r '[.messages[] | select(.role == "user")] | last | .job_state')
    case "$state" in
      pending | running) ;;
      *) say "  $state: $content"; return ;;
    esac
  done
  say "  still $state after ten minutes: $content"
}

session() {
  call POST /v1/agent-sessions "$(jq -n --arg a "$1" '{agent_id: $a}')" | jq -r .id
}

front_desk=$(agent_id front-desk)

# Dates spelled out, two weeks ahead: "next Friday" is ambiguous enough that a
# model asks which one, and the conversation stops at the question.
day() { python3 -c "import datetime as d; t = d.date.today(); f = t + d.timedelta(days=(4 - t.weekday()) % 7 + 14 + $1); print(f.strftime('%A %-d %B %Y'))"; }
arrive=$(day 0)
leave=$(day 2)

s=$(session "$front_desk")
ask "$s" "Which rooms do we have, and what does each cost a night?"

s=$(session "$front_desk")
ask "$s" "Is the Orchard Room free from $arrive to $leave?"

s=$(session "$concierge")
ask "$s" "A guest is asking what there is to do nearby on a rainy afternoon."

# Booked in one message and charged in the next: asked together, the turn
# resumed after the approval re-books and finds the room taken by its own
# booking (ui/e2e/approvals.mjs). Left held, for the inbox to show.
s=$(session "$front_desk")
ask "$s" "Book the Garden Room for Jo Okafor, arriving $arrive and leaving $leave."
ask "$s" "Charge that booking to the Visa she has on file with us."
# The one thing the demo cannot do without. A model that asked a question
# instead, or a room already booked by an earlier run -- Hollowbrook keeps its
# bookings in memory until its pod restarts -- leaves the inbox empty.
[ "$state" = parked ] || { say "the charge was not held for approval ($state); see the last session"; exit 1; }

say "done"
