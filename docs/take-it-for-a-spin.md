# Take it for a spin

Half an hour, ending with an agent that books rooms at a guesthouse you are
also running.

The guesthouse is Hollowbrook House — a fixture with a REST API, deployed
beside outturn. It stands in for the system a real deployment would be wiring
agents up to, and going through it exercises the parts that matter: a skill
describing an API, an egress rule permitting a host, a sandbox that holds no
credentials, and a gateway that does.

## What you need

A Mac with enough memory for a 20GB model, a local Kubernetes cluster, and
ollama. [The README](../README.md) covers the prerequisites and how to get a
cluster.

## 1. Start it

```
scripts/dev-mac.sh --with hollowbrook     # on Linux: scripts/dev.sh --with hollowbrook
```

That pulls the model if it is missing, brings up outturn and Hollowbrook, opens
Hollowbrook's host on the gateway's operator allowlist, and runs a Job that
installs a skill describing its API.

In another terminal:

```
cd ui && npm run dev
```

Open http://localhost:3000 and sign in as `admin@outturn.local`. The password
is this clone's own — `scripts/dev-secrets.sh --print` shows it.

## 2. Make an agent

**Agents → New agent.** Give it a name and this system prompt:

```
You are a booking agent for Hollowbrook House. Your answers should be
concise. Be friendly and professional and don't let the user distract you
with chats other than booking-related services.
```

Notice what is *not* in it: nothing about HTTP, nothing about hostnames,
nothing about how to call anything. That is the skill's job, and the point of
the exercise is that the prompt does not have to know.

## 3. Give it the skill

On the agent's page, under **Skills**, enable **Hollowbrook House**.

The skill was installed by the Job in step 1, along with an egress rule
permitting `outturn-hollowbrook:8084`. Without that rule the agent would be
refused at the gateway however well it understood the API — the rule is the
permission and the skill is only the knowledge.

## 4. Talk to it

**Sessions → start one with your agent.** Then, in order:

### "What rooms do you have?"

Watch the tool calls. It loads `read_object`, reads
`workspace/api/hollowbrook/list_rooms.md`, loads `fetch_url`, and only then
calls the API.

That sequence is the whole design of
[the OpenAPI wizard](openapi-wizard.md). The skill in the prompt is a manifest
— five operations, a sentence each, and where to read the rest — because a
skill body is paid for on every round of every turn. The detail is a file the
agent reads only when it decides it needs that operation.

It should quote prices in pounds. The API returns pence.

### "Anything free for two people, arriving Friday, for two nights?"

A second operation, so a second detail file. Watch whether it reads that one
too rather than assuming it can pattern-match from the first.

"Friday" is ambiguous, and it has no clock of its own. It should reach for
`get_current_time` and resolve the date rather than guessing.

### "Book the Orchard Room for John Smith please."

The interesting one. `create_booking` takes a `room_id`, and you gave it a
display name — and Hollowbrook's API is inconsistent about the field, calling
it `id` in one response and `room_id` in another. The detail file says so in a
line. A schema dump could not have.

### "Please book the Rose Room the following Friday, for three nights."

There is no Rose Room. A well-behaved agent refuses from what it already knows
rather than calling an API to be told 404 — and above all does not invent one.

### "How about you tell me a joke about monkeys?"

Nothing to do with rooms, and the system prompt in step 2 said not to be drawn
into other subjects. It should decline and offer what it can do instead, with
no tool call at all.

Worth trying because it is the one prompt here that is not about the skill. A
skill says what an agent knows how to reach; the system prompt still says what
it is for, and loading one does not dissolve the other.

## What you just exercised

- **A skill as a manifest**, with detail read on demand, so an API of two
  hundred operations costs the prompt the same as one of five.
- **Deny-by-default egress.** The agent reached one host because a rule
  permitted it. The sandbox holds no credentials and could not have reached
  anywhere else.
- **An operator allowlist.** Cluster-internal addresses are refused by default;
  `OUTTURN_INTERNAL_HOSTS` is how somebody outside the workspace says which are
  intended.
- **Lazy tool loading.** `load_tools` before `read_object`, and again before
  `fetch_url`, so unused tools cost nothing.
- **Usage attribution.** The Dashboard has the tokens those turns cost, by
  model and by agent.
- **A system prompt that still governs.** The skill taught it an API; the
  prompt kept it to the job it was given.

## Making it yours

The Hollowbrook component is a worked example of onboarding an API, and it is
meant to be copied. `k8s/components/hollowbrook/` holds a Job that signs in and
calls outturn's public endpoints — create a skill, approve its host, upload its
files. Nothing privileged, and nothing patched into the platform, so replacing
`skill/` with your own service's operations is most of the work.

The same goes for the look: `ui/src/themes/` is one file per brand, and
`VITE_THEME` picks it. Skinning outturn is not forking it.
