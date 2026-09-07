# Settings

How defaults cascade from the operator to tenants to agents, and who may
change what. Built: the catalogue is `src/api/settings/mod.rs`, the rows are
`setting_overrides`, and the walk is `SettingsStore::resolve`, called once per
turn in `prepare_turn`. The first three entries are temperature, reasoning
effort and model calls per turn.

## The shape

A **catalogue in code** and **overrides as rows**. The same split as
authorities and roles.

The catalogue lists every setting: its key, its type, its default, and who may
override it. It changes when the code changes, because a setting nothing reads
is noise and a value nothing names cannot be set.

The rows hold overrides, one per level that has chosen to differ:

```
system   (operator)   temperature = 0.3     the default everyone inherits
tenant   (HOA Co)     temperature = 0.1     override on: a row exists
agent    (Invoicer)   --                    override off: no row, inherits
```

Resolution walks up: the agent's row, else the tenant's, else the system's,
else the catalogue default. Turning an override off deletes the row, so that
level falls back to whatever is above it. There is no "copy the default down"
step, which is what lets an operator change a system value and have it reach
every tenant that never chose otherwise.

## Who may override

Part of the catalogue, not of the roles. Each setting is either
*operator-only* or *tenant-overridable*, and a tenant-overridable setting may
also be overridden per agent.

- Temperature, reasoning effort, max tool rounds: tenant-overridable.
- Model routing: operator-only. The operator certified a workflow against a
  model and pays for it; a tenant switching models breaks both. A tenant that
  brings its own key gets model choice within what the operator has certified,
  and that is a routing feature (see [routing.md](routing.md)), not a setting.
- Storage retention per scope, when built: operator-only default, tenant may
  shorten.

## What the UI does with it

A settings page reads the catalogue and shows each setting's *effective*
value and where it came from: "platform default", "set by this workspace",
"set for this agent". Beside each tenant-overridable setting is an **Override**
checkbox. Off, the value is shown greyed and inherited. On, it becomes
editable and a row is written. The agent editor uses the same component for
agent-level overrides.

The point is that a tenant who does not know what temperature is never sees a
blank number they are expected to invent. They see a value that is already
right and a checkbox they can leave alone. Only the operator has to understand
every setting, and the operator is the one who set the defaults.

## Skills as a level

When skills exist, a skill sits between tenant and agent in the walk and may
pin settings: an invoice skill certified at low temperature and high reasoning
effort declares that, and an agent running the skill gets those values however
the tenant configured itself. A skill's pins are declared in its frontmatter,
because they are facts about the skill; who is billed for running it is not,
and never belongs there.

## What is in it

Temperature, reasoning effort and model calls per turn, moved out of the
agent's free-form `policy` JSON, where they had no defaults above the agent and
no way for an operator to set them once. Storage retention comes when scopes
do. The endpoints are `/v1/settings` (tenant), `/v1/platform/settings`
(operator) and `/v1/agents/{id}/settings` (agent), each returning every
setting with its effective value, where it came from, what this level would
inherit, and whether this level has its own row. PUT sets a row, DELETE
removes it.

## What is deliberately not a setting

- Anything the agent could read or change. The sandbox receives resolved
  values on the turn and nothing else, the way it receives credentials.
- Authorities and roles, which have their own tables and their own rules.
- Routing rows, which are ordered lists with credentials attached, not scalar
  values, and are documented separately.
- Memory. "This customer pays three months late" is a fact the agent learns,
  scoped to a tenant, stored as an agent- or tenant-scope file. It is read
  into prompts, not resolved as a default.

A generic key-value store with a generic editor would be the quick way to
build this and the wrong one: the catalogue is what keeps the page honest
about types, defaults and who may touch what. Purpose-built features that
happen to store their "who may override" bit in one place is the intended
layering.
