# Take it for a spin

Half an hour, ending with an agent that takes bookings and payments at a
guesthouse you are also running -- and that stops and waits for a manager
before it charges anybody.

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
cd ui && VITE_THEME=hollowbrook VITE_BRAND_NAME='Hollowbrook House' \
  VITE_BRAND_LOGO=/hollowbrook-logo.svg npm run dev
```

Those three are why the next half hour looks like a guesthouse's own software
rather than like outturn. Vite reads them at build time, so they are set on the
command that starts it rather than configured in the app -- which is also why
`--with hollowbrook` cannot set them for you: the UI runs on the host and the
component installs into the cluster. `ui/src/themes/README.md` has the rest.

Plain `npm run dev` gives you outturn's own look, which is worth seeing once:
anything that survives both is genuinely coming from a token, which is what the
second theme exists to prove.

Open http://localhost:3000 and sign in as `admin@outturn.local`. The password
is this clone's own — `scripts/dev-secrets.sh --print` shows it.

## 2. Make an agent

**Agents → New agent.** Give it a name and this system prompt:

```
You help the staff of Hollowbrook House, a guesthouse, with bookings and
payments. Whoever you are talking to works at the house and usually has a
guest on the phone or in front of them, so answer them as a colleague would:
briefly, and with what they need to say next. Keep to the house's business.
```

Notice what is *not* in it: nothing about HTTP, nothing about hostnames,
nothing about how to call anything. That is the skill's job, and the point of
the exercise is that the prompt does not have to know.

Notice who it says the user is, too. This platform has seats for the people
who run a deployment and for the employees of its tenants, and deliberately
none for a tenant's own customers -- so an agent here works *for* a member of
staff, on the house's systems, and the guest is what the conversation is about
rather than who is in it. A prompt written as though the guest were typing
describes somebody this platform has nowhere to put, and the conversation
reads oddly ever after.

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

Keep the booking id it quotes back; the next step needs it.

### "Put it on their card please, the Visa ending 4471."

Where the half hour has been going. Watch three things happen in order.

It reads `list_payment_accounts` and finds `pa_4471` -- a *payment account
number*, which stands for a card the house holds and is not a card number.
Nothing in this API returns one, which is deliberate: a transcript is replayed
to a model on every later turn, so a card number that reached one would be
there for good.

It reads `charge_payment_account`, and sends the booking's own `total_pence`
rather than a figure it worked out. The house refuses more than a booking's
total anyway, which is the guard for an agent that multiplied a nightly rate
itself.

And then the conversation stops. The reply says it is putting the charge
through, and the thread shows it **paused** rather than failed -- a deliberate
hold, not an error. Nothing was charged.

### "Please book the Rose Room the following Friday, for three nights."

There is no Rose Room. A well-behaved agent refuses from what it already knows
rather than calling an API to be told 404 — and above all does not invent one.

### "Can you tell me a joke about monkeys?"

Nothing to do with the house, and the system prompt in step 2 said to keep to
its business. It should decline and offer what it can do instead, with no tool
call at all.

Worth trying because it is the one prompt here that is not about the skill. A
skill says what an agent knows how to reach; the system prompt still says what
it is for, and loading one does not dissolve the other.

## 5. Answer it

The charge needed somebody's say-so, and that somebody is you.

`charge_payment_account.md` opens with frontmatter saying so:

```yaml
---
approval:
  requires: charge
  matches: POST /charges
  covers: booking
  identified_by: booking_id
---
```

That is a rule about an operation, living in the file that documents the
operation -- versioned with the skill, immutable once published, and editable
only under `skills:write`. [docs/approvals.md](approvals.md) is why there
rather than on the agent, in a setting, or on the egress rule that permits the
host.

And the gateway is what stopped the charge, not the agent's good manners. When
the skill was published, the API recorded that `POST /charges` on Hollowbrook's
host needs a "charge"; when the turn started, it hashed that into the turn
token; and when the agent asked the gateway to make the call, the gateway
checked the rules it was offered against that hash and refused. A runtime that
dropped the rule from its copy gets nowhere, because the copy it can rewrite is
not the one the token vouches for -- the same move that makes egress rules worth
anything.

The request is waiting in the action queue, addressed to a role rather than to
a person: who may approve a charge is a question about the house's own
organisation, and it changes without the pending request changing. Approve it,
and the turn you left parked is given back to the queue and runs -- the charge
goes through, and the agent tells the person who asked.

Decline it instead and the hold stays on. The conversation stays paused, which
is honest: nothing has changed about whether the charge may happen.

Worth doing twice, as two people. Sign in as somebody whose role does not carry
`approvals:answer` and the request is visible in their queue and unanswerable,
which is the difference between being asked and being entitled.

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
- **A rule that travels with the prose that documents it.** Frontmatter on the
  operation's own file, versioned with the skill and editable only under the
  skill's authorities, rather than a setting somewhere else that could disagree
  with what the agent was told.
- **A turn paused rather than failed.** The conversation stopped where it was
  consistent, kept its place, and carried on when somebody answered -- which is
  a different thing from a turn that errored and was retried.
- **An approval addressed to a role.** Not to a person, so who may answer
  changes when the house reorganises and the pending request does not. And an
  authority to answer that is separate from being asked.
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

The same goes for the look, which step 1 already had you run: `ui/src/themes/`
is one file per brand and `VITE_THEME` picks it, while `VITE_BRAND_NAME`,
`VITE_BRAND_LOGO` and `VITE_BRAND_TAGLINE` carry the name and the mark. Skinning
outturn is not forking it -- `src/lib/brand.ts` says why those are build
variables rather than source, and `Logo.tsx` is the one place the mark is drawn.
