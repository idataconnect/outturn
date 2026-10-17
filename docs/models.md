# Models

Which models a deployment can use, how they are chosen and chained, how each
is configured, and how the platform keeps working as models are released,
renamed and retired. Designed; replaces [model-catalog.md](model-catalog.md)
and the parts of [routing.md](routing.md) and [settings.md](settings.md) that
dealt with models. Nothing here is built.

## Why redo it

What exists is halfway between two designs. A model is named statically --
`OUTTURN_DEFAULT_MODEL`, or a name in an agent's policy -- and routing is a
table nothing edits but SQL, keyed by a credential that is an environment
variable. Settings that only mean something to a model -- temperature, how
hard to think -- cascade operator to workspace to agent with no idea which
model they will reach. And what a model accepts is guessed from its name.

Each of those has already failed in front of somebody:

- Sonnet 5.5 refused every turn because a platform temperature, set for a model
  that took one, reached a model that does not.
- Pointing local development at a hosted model was a hand-written `insert`, and
  it silently retired the local model with no fallback.
- The stored reply said `qwen3.5` -- the deployment's default -- while the
  ledger showed Sonnet had answered, because what the agent asked for is not
  what a route sends.
- Gemini 3.7 Flash went generally available in August 2026 and was described as
  the previous generation, routed automatically to its successor, within weeks.
  A deployment that pinned it would have had months, at most, before its
  configuration named something that no longer answers.

The platform must not break because a provider retired a model, and nobody
setting it up should need to know which parameters which model refuses.

## The principles

1. **Configuration lives in the database**, edited through the UI, effective on
   the next request. A model change is never a deploy.
2. **The catalog says what a model accepts.** The UI offers only what the chosen
   model supports, so an invalid configuration cannot be entered.
3. **Settings belong to a model.** Temperature and thinking are configured per
   model, at each level, so a fallback to a different model uses *its* settings.
4. **Every use is a chain.** One model is a chain of one. Recipes make the
   common reliable shapes one click.
5. **Nothing stops working on its own.** A model that disappears is a failure
   like any other, and the chain carries on. A tracked model moves forward only
   when the settings on it remain valid, and otherwise waits for a person.
6. **Record what actually served.** Not what was configured or asked for.

## The catalog

[models.dev](https://models.dev) is the source: an MIT-licensed catalog of a few
hundred providers and several thousand models, one JSON document at
`https://models.dev/api.json`. Checked against it in October 2026:

| Field | Coverage | Used for |
|---|---|---|
| `temperature` | most | Whether sampling parameters may be sent |
| `reasoning`, `reasoning_options` | most / ~75% | Thinking: a toggle, effort levels, or both |
| `tool_call`, `attachment`, `structured_output`, `modalities` | most | What a request may carry |
| `limit.context`, `limit.output` | all | The trim's budget, and `max_tokens` when none is named |
| `family` | ~92% | Grouping versions into a line, for "Latest" |
| `release_date` | all | Ordering a line, newest first |
| `canonical_model_id` | ~68% | The same model at different providers (`openrouter/google/gemini-3.7-flash` and Vertex's copy are both `google/gemini-3.7-flash`) |
| `status` | ~4% | `deprecated`, `beta`, `alpha`, where it says |

**Not taken: `cost`.** outturn reports tokens, fuel and duration, never money
([usage.md](usage.md)); a price in the catalog would be a number somebody
mistakes for the bill. It is dropped when the catalog is stored.

**What it does not say.** No retirement dates and no successors, and its
`status` lags: Gemini 3.7 Flash had no status on any of its ten listings when
Google already described it as superseded. So deprecation in the catalog is an
early warning when it is there, never the guarantee. The guarantee is the chain
(*Nothing stops working*, below). Nor does it carry everything the gateway needs
-- whether forced tool choice or mid-conversation system messages are accepted
-- so the catalog entry is completed by **family defaults** held in code, and
any field can be overridden per configured model.

**Shipped with the image.** A snapshot of the catalog, reduced to the fields
above, is built into every release -- a few hundred kilobytes. A deployment
starts with a catalog whether or not it can reach anything: first run needs no
fetch, an offline install works as shipped, and tests run against a known
catalog rather than whatever the site said that morning. Fetching is how a
deployment stays current, not how it starts.

**Fetched rarely, and only when it changed.** The site is somebody else's free
service, and new models arrive weekly at most. So:

- A scheduled job checks once a week, at a time offset per deployment so
  every install does not ask in the same minute.
- Every check is conditional (`If-None-Match`, `If-Modified-Since`): a
  catalog that has not changed costs the site a "not modified" and nothing
  more.
- An operator can ask for a refresh from the Platform page, limited by the
  server to once an hour.
- Nothing else fetches. A turn, a route and the chain editor read what is
  stored; the catalog is never on the path of a request.

The fetch goes through the gateway, the tier that reaches the outside. Each
one that brought something new is kept as a snapshot:

```
catalog_snapshots (id, fetched_at, source_url, sha256, models jsonb)
```

`models` is the document reduced to the fields above, keyed by provider and
model id. Configured models record which snapshot their facts came from, so a
refresh never changes a model under anyone, and an entry that leaves the
catalog keeps the facts it had.

**Where it comes from is a setting.** The source URL defaults to the site and
can name a mirror -- an internal copy, an object in a bucket -- and fetching
can be turned off altogether. A deployment managed from a repository can also
point a setting at a file (a mounted ConfigMap, say), loaded at startup as a
snapshot like any other, so the catalog is versioned with everything else
rather than uploaded through a page.

**A deployment that cannot reach the internet** -- air-gapped, or with egress
locked down -- still has the shipped snapshot, only older. The Platform page
says how old, and offers the same two ways on as everywhere else: import a
catalog file (the same JSON, carried in by hand or mounted), or enter models
by hand. A model served locally -- ollama, llama.cpp, a vLLM -- is never in the
catalog under the name it is served as, so entering one by hand, with its
capabilities typed in, is a first-class path and not a fallback.

## The objects

```
provider_accounts   where requests go: protocol, base URL, credential, owner
models              a configured model: an account, and what to ask it for
chains              an ordered list of models, tried in turn
```

**Provider accounts.** A protocol (`anthropic`, `openai`, `gemini`, and
`bedrock` when built -- below), a base URL, and a credential. Owned by the
platform or by a workspace that brings its own key. This is today's
`traffic_routes` endpoint half, given a row of its own so two models at one
provider share one key.

**Models.** An account, and either a **pinned** catalog model --
`claude-sonnet-5-5` -- or a **tracked** line -- *Sonnet (Latest)*, a `family`
at that provider. A tracked model stores the version it currently resolves to,
and a pending version when an upgrade is waiting on somebody (below). A
display name, a link to its catalog snapshot, and overrides of any catalog
field. A hand-entered model has no catalog link and its fields are all typed.

**Chains.** An ordered list of models. A request walks it: the first model, and
on failure the next. Each link after the first is marked **same model** --
same `canonical_model_id`, a different door -- or **different model**, which
the UI shows with its caveats. Owned by the platform or a workspace.

**Who uses which chain.** Each traffic class (`assistant`, `background`,
`session-name`, as in [routing.md](routing.md)) has a platform default chain,
which a workspace may replace with its own. An agent may name a chain of its
own instead of its class's -- this replaces `model` in an agent's policy. A
chain is the only way anything names a model, so there is one place a model is
chosen and one place a fallback is defined.

## Settings belong to a model

The settings that mean something to a model move from three levels to three
levels *per model*:

```
platform  × model      Sonnet (Latest): effort high
workspace × model      Kestrel, Sonnet (Latest): effort medium
agent     × model      Invoicer, Sonnet (Latest): --   (inherits medium)
```

The same cascade as [settings.md](settings.md) -- agent, else workspace, else
platform, else the default -- with the model as part of the key, and a value
for every model still possible at each level. Within a level the model's own
value wins; between levels the nearer wins: agent and model, agent, workspace
and model, workspace, platform and model, platform. A value for every model
that one model cannot take -- a temperature, on a model that refuses one -- is
passed over for that model, and the next value down is used.
`setting_overrides` gains a `model_id`, nil (as `agent_id` is) for a value that
holds for every model, which is every row that existed before. Which settings are model-scoped is part of the setting's catalog entry:
`temperature`, `reasoning_effort` and `context_budget` are; `max_tool_rounds`,
`session_naming` and the rest are not.

What this buys:

- **A fallback uses the fallback's settings.** A turn that falls from Sonnet to
  Gemini sends Gemini's effort and temperature, never Sonnet's.
- **Only valid values exist.** The setting form for a model offers what its
  catalog entry allows: no temperature for a model that refuses one, its own
  effort levels, a context budget no larger than its window. Saved values are
  validated on the server against the same entry.
- **The model's default is the real default.** Leaving a setting alone means
  the model's own default, from the catalog or its family, not a platform
  number chosen for some other model.

A setting on a *tracked* model attaches to the model row, not to a version, so
it survives an upgrade -- which is why upgrades validate it (next section).

## Tracking the latest version

A tracked model resolves to the newest version in its line at its provider:
same `family`, latest `release_date`, not `alpha` or `beta` and not
`deprecated` unless nothing else is left. Each catalog refresh recomputes it.

When the newest version changes, every setting on the model -- at every level,
for every workspace and agent -- is validated against the new version:

- **Nothing set, or every set value still valid:** the model moves forward. A
  value left at the default takes the new version's default.
- **A set value the new version refuses** -- a temperature on a model that
  takes none, an effort level it does not offer: the model **stays on the
  version it is on**, which still works, records the pending version and why,
  and asks whoever may configure it. Approving applies the upgrade and the
  adjusted values together, in one step; the adjustments are proposed --
  nearest valid level, or the field cleared -- and shown before anybody
  approves. Until then nothing changes.

The question goes to the action queue ([action-queue.md](action-queue.md)) of
whoever holds the authority to configure that model -- the operator for a
platform model, the workspace's administrators for their own -- and to the
Platform page.

A **pinned** model never moves. When a newer version exists in its line, the
same people are told, at low priority, with the upgrade one click away.

**Between turns, never between rounds.** A version changes when a turn starts,
not partway through one: a thinking block is valid only for the model that
wrote it, and the cache is per model, so a turn that crossed versions would
lose both mid-loop. A conversation that crosses an upgrade loses them once, at
the boundary.

## Nothing stops working

A model can vanish without warning -- retired, renamed, or redirected to its
successor. The platform's answer is the chain, and four rules make it hold:

1. **A refused model is a failure of that link, not of the request.** Today a
   4xx stops the walk, on the reasoning that a request the provider refused
   will be refused by the next one too (`is_client_error`, `router.rs`). That
   holds for a malformed request and fails for a model that no longer exists,
   which answers 404 or 400 and ended the turn. The gateway tells them apart --
   "model not found", "deprecated", "no longer available" in the provider's own
   error is the link's fault, and the walk continues -- and a model refused
   this way trips its link's breaker for a day, so later turns go straight to
   the next.
2. **Every recipe ends in a fallback.** The first-run setup does not offer a
   chain of one for a hosted model without saying what happens when it goes.
3. **Deprecation is announced before it bites,** where anything says so: a
   catalog entry turning `deprecated`, or a link that keeps tripping its
   breaker, raises an item for whoever configures it.
4. **What served is recorded.** A provider that answers a request for 3.7 Flash
   with 3.8 has changed the model without an error; the served model, which
   the gateway already reports per call, is stored on the reply and the ledger,
   and a served model that differs from the configured one is shown as such.

## Credentials

**Decision to confirm: keys are sealed credentials in the database, with the
environment still honored.** Configuring a chain in the UI needs its keys
entered in the UI, or the first-run setup stops at "now edit a manifest and
redeploy". Storing a key in the database is safe in the way that matters:
[sealed credentials](sealed-credentials.md) are encrypted to the gateway's key
with the host they may reach sealed in beside them, so anyone who can read or
write the database can neither read a key nor point it elsewhere. That
document said provider keys stay in the environment because changing provider
was a deploy anyway; this design is what makes that untrue.

A provider account may still name an environment variable instead, for an
operator whose secrets are managed outside outturn (a Kubernetes secret
synced from a vault). The account form offers both; an environment variable
is read at gateway start, so changing it is still a roll.

## First run

The first person to sign in as the operator, on a deployment with no models
configured, is taken to setup. It is skippable, and reachable later from the
Platform page.

1. **The catalog** is the shipped snapshot or something newer, with its age
   shown, and the file import and hand entry offered beside it.
2. **A recipe**, each a chain with its accounts and models filled in:

   | Recipe | Chain | Caveats shown |
   |---|---|---|
   | Claude, two doors | Sonnet (Latest) on Bedrock → Sonnet (Latest) on Anthropic | none: same model |
   | Claude, direct | Sonnet (Latest) on Anthropic → Haiku (Latest) on Anthropic | smaller model on fallback |
   | Fast first | Gemini Flash (Latest) → Sonnet (Latest) | different family |
   | Mixed vendors | Sonnet (Latest) → Gemini Pro (Latest) | different family |
   | Local | a hand-entered model on ollama or llama.cpp | no fallback: says so |
   | Start empty | -- | -- |

   Every recipe can be reversed or edited before it is saved.
3. **Keys**, one per provider the recipe uses, sealed in the browser.
4. **A test call** through the chain, which shows what answered.

"Different family" caveats say what changes when the chain falls over: the
answer's voice and quality, the thinking and the cache starting again, and
whether skills certified on one model have been tried on the other.

## The chain editor

The page a beginner has to understand at a glance. A chain is drawn as cards
left to right, joined by arrows labeled with what moves a request along --
"if unavailable", "if rate limited", "if refused" -- each card showing the
model, its provider, whether it is tracking "Latest" and what that currently
means, and a badge where a link changes model family. A card's settings open
beside it and offer only what that model supports. Drag to reorder; a chain's
traffic classes and agents are listed under it, so changing it shows what it
reaches.

## Requests

What the gateway does per call, replacing what it does now:

- **Resolve** the request's chain: the agent's own, else the workspace's for the
  class, else the platform's.
- **Walk** it: per link, the resolved model's facts (catalog entry, family
  defaults, overrides) and the effective settings for that model at the
  request's agent and workspace, applied by the link's protocol -- sampling
  parameters left out where refused, effort mapped to what the model offers,
  `max_tokens` at its output limit, thinking as [thinking.md](thinking.md) has
  it. This replaces `ClaudeTraits` and every name match in `translate.rs`.
- **Fall over** per call, never mid-stream, on the failures the link owns: a
  connection or timeout, a 5xx, a rate limit, a refused model. A request the
  provider refused for being malformed is refused everywhere and ends the walk.
- **Record** the served model, the account, the chain position and whether the
  model changed, on the ledger and the reply.

The runtime and the guest name no model. A turn asks for its class; the chain
decides.

## What changes, and what migrates

| Today | Becomes |
|---|---|
| `OUTTURN_DEFAULT_MODEL` | Removed after migration. On first start of the new version, if set and no models are configured, it seeds a hand-entered model on the deployment's OpenAI-protocol account and a chain of one, so an existing deployment keeps answering. |
| `traffic_routes` | Migrated: each distinct endpoint and credential becomes a provider account, each row a model, each class's rows a chain in priority order. |
| `credential_ref` env vars | Kept as an account's credential option; new accounts default to sealed. |
| `temperature`, `reasoning_effort`, `context_budget` overrides | Copied onto every configured model where valid for it; a value that is invalid for a model is dropped for that model and reported once. |
| Agent and template `policy.model` | Becomes a chain reference: an agent that named a model gets a chain of one for it. |
| `ClaudeTraits` in `translate.rs` | Deleted; facts come from the catalog and family defaults. |
| `agent_messages.model` | Written from the served model, never the requested one. |

## Tests

Most of the risk is in time passing, so most tests are about it.

- **Upgrades:** a tracked model with defaults moves forward; with a still-valid
  override moves forward and keeps it; with an override the new version
  refuses stays put, raises an item, and moves with the adjusted value only
  when approved; overrides at agent and workspace levels are found, not just
  the platform's; a version that disappears from the catalog while pending.
- **Retirement:** a link answering "model not found" falls over to the next and
  trips its breaker; a malformed request does not fall over; a silent redirect
  is recorded as a different served model.
- **Settings:** a fallback to another model sends that model's settings; the
  server refuses a value the model does not accept; a model's own default is
  used when nothing is set.
- **Catalog:** a first start with no network uses the shipped snapshot; a
  check of an unchanged catalog stores nothing; refusal of a refresh within
  the limit; a mirror URL and a mounted file each load as a snapshot; fetching
  turned off fetches nothing; a model leaving the catalog keeps its facts.
- **Migration:** each row of *What changes* above, against a database shaped
  like today's.

## Order of work

Each phase leaves the platform working.

1. **Served model.** Record what served on the reply. Built: the runtime
   reports the model the gateway said answered the turn's last round, and the
   reply stores that or nothing -- never the configured one. The ledger
   already had it per call. Showing a served model that differs from the
   configured one is left for the chain editor, which is where the
   configured one will be visible to compare against.
2. **Retirement falls over.** Built. A 404 or 410 that is about the model,
   or a 400 that says so outright ("model not found", "the provided model
   identifier is invalid", "has reached the end of its life"), is the route's
   failure and the walk goes on; any other 4xx still ends it. The model then
   has a circuit of its own -- the endpoint and the model together, so
   nothing else the endpoint serves is affected -- which one such answer opens
   for a day (`RETIRED_FOR`), after which a single call checks again.
3. **Catalog.** Built. `assets/catalog.json.gz` ships in the image
   (`scripts/refresh-catalog.sh` rebuilds it) and is kept as the first
   snapshot; `OUTTURN_CATALOG_FILE` loads a mounted file at startup; the
   worker's tick checks `OUTTURN_CATALOG_URL` weekly, conditionally, through
   the gateway, unless `OUTTURN_CATALOG_FETCH=off`; Platform → Models shows
   what is held, checks on request at most hourly, and imports a file.
4. **Accounts, models, chains.** Built. Migration 0041 gives each its table
   and carries `traffic_routes` over -- each endpoint and credential an
   account, each enabled row a model, each class's rows a chain in priority
   order -- and at startup a model nobody has described takes the newest
   catalog's entry, found by the account's host (`catalog::provider_for`).
   The gateway walks the chain for the request's class; each link carries
   its model's facts (`outturn_core::facts`), which replace `ClaudeTraits`:
   sampling, the output limit and the effort levels from the catalog, and
   adaptive thinking and `block_binding` as the Claude family's defaults by
   release date, since the catalog does not say them and Opus 4.5 lists
   effort levels but thinks by budget. An account's key is a variable in the
   gateway's environment or sealed as a `provider` key -- a seal kind the
   egress path refuses, so a platform key cannot be attached to an agent's
   request -- bound to the account's host; `outturn-seal --provider` makes
   one. The operator configures all of it under `/v1/platform`.

   Left for later, and why:

   - **A workspace's own** accounts, models and chains: the schema has them,
     the API waits on who may configure them (*Open questions*).
   - **An agent naming a chain**, which replaces `policy.model`: an agent can
     already name a traffic class, and a class can be given any chain, so
     nothing is lost meanwhile.
   - **`OUTTURN_DEFAULT_MODEL`** and the gateway's static providers stay
     until first-run setup (phase 7) can replace them. A class with no chain
     still falls to the static providers with the requested model, as an
     unrouted class always has. `traffic_routes` is kept, unread, for a
     release.
5. **Model-scoped settings.** Built. `?model=<id>` on the settings routes
   reads and writes one model's values, and a value saved for one model is
   checked against its facts (`model_settings::valid_for`); `/v1/models`
   lists the models a workspace may choose for. Today's overrides were not
   copied onto each model, as first planned: they stay, as values for every
   model, which leaves the settings page working until it can name a model
   (phase 7) and gives a model configured later the deployment's preference
   rather than nothing.

   Resolved where the model is known. The gateway resolves effort and
   temperature per attempt (`outturn_core::model_settings`), for the agent
   the session belongs to, so a fallback is asked with its own model's
   values; the API no longer sends either, and what a caller names itself
   still stands. The conversation's size is decided before any model is
   asked, so it is the smallest budget any model on the chain resolves to --
   a fallback to a smaller window never finds the conversation too long --
   and prompt repetition is the first model's. A template's fixed value wins
   wherever the model can take it. "The model's own default" is the
   setting's: the catalog says nothing about default efforts.
6. **Tracking "Latest".** Built (`api::tracking`), run whenever a catalog is
   kept and at startup. A line is narrower than a catalog family, which is
   coarse -- Gemini Pro's holds image, research and translation models -- so
   the next version must also be what the current one is: the same output,
   at least the same input, tools and reasoning where it has them, and the
   same id up to the family's name, which keeps Bedrock's `us.` ids among
   `us.` ones. A provider's moving alias (`-latest`) is never chosen, nor a
   preview unless the current model is one (`catalog::newest_in_line`).

   A tracked model with nothing set against the new version moves; one with
   a value the new version refuses waits, and so does a pinned model with a
   newer version out. Either way `pending_model` says what is waiting, why,
   and what approving would change (`model_settings::adjust`), and an
   `model.upgrade` item asks the operator. `POST
   /v1/platform/models/{id}/upgrade` approves it, refused with 409 if the
   values it would change are no longer the ones shown. The operator is
   asked in the platform's own workspace, which the inbox admits for system
   administrators whichever workspace they have open. A workspace's own
   models will ask its administrators, once it can have any.

   A turn finishes on the version it began on: a moved model keeps its
   previous version and facts, and the gateway asks for that one when the
   turn's reply -- a UUIDv7, so it says when the turn began -- is older than
   the move.
7. **The UI:** the chain editor, per-model settings, then first-run setup and
   recipes.
8. **Bedrock**, which the "two doors" recipe needs. Begun: a `bedrock` route
   protocol for Claude through Invoke -- the Anthropic body and events, so the
   same model behaves the same through either door -- authenticated with a
   Bedrock API key, and AWS's event-stream framing decoded
   (`provider/bedrock.rs`), and signed with AWS keys (SigV4,
   `provider/sigv4.rs`) when a route names no API key: the gateway's
   `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN`, for the
   region in the endpoint's host or `AWS_REGION`. Not yet: a role assumed
   through a pod's web identity, and a real call against AWS.

   **How it is tested without an AWS account.** Three parts, each proving
   what the others cannot:

   - *The protocol*, against MiniStack (MIT, a local AWS emulator whose
     Bedrock runtime streams Invoke in the real event-stream format, with
     Anthropic-shaped events for `anthropic.*` ids). It runs as an optional
     local component, `scripts/dev.sh --with ministack`, at zero replicas
     until wanted, with `MINISTACK_BEDROCK_PROXY_URL` pointing at a local
     OpenAI-protocol model so a real turn gets real text through a
     Bedrock-shaped door.
   - *Signing*, offline: SigV4 is deterministic, so the signer is checked
     against AWS's published SigV4 test vectors and against AWS's own Python
     signer (`botocore`'s `SigV4Auth`) on a Bedrock-shaped request with made-up
     keys and a fixed time. MiniStack cannot do this: it parses a signature
     to find the caller and accepts any.
   - *AWS accepting real credentials* is the one thing left for a real call,
     by whoever first has an account to make it with.

## Open questions

- **Bedrock:** Invoke is the way in, and is begun with API keys. Whether AWS
  request signing is needed before the recipe is offered, or API keys are
  enough to start.
- **The recipe list:** the table above is a first draft.
- **Who may configure a workspace's models:** a new authority, or the existing
  workspace settings one ([authorities.md](authorities.md)).
