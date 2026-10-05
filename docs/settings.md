# Settings

How defaults cascade from the operator to workspaces to agents, and who may
change what. Built: the catalog is `src/api/settings/mod.rs`, the rows are
`setting_overrides`, and the walk is `SettingsStore::resolve`, called once per
turn in `prepare_turn`. The first three entries are temperature, reasoning
effort and model calls per turn.

## The shape

A **catalog in code** and **overrides as rows**. The same split as
authorities and roles.

The catalog lists every setting: its key, its type, its default, and who may
override it. It changes when the code changes, because a setting nothing reads
is noise and a value nothing names cannot be set.

The rows hold overrides, one per level that has chosen to differ:

```
system      (operator)   temperature = 0.3     the default everyone inherits
workspace   (Kestrel)    temperature = 0.1     override on: a row exists
agent       (Invoicer)   --                    override off: no row, inherits
```

Resolution walks up: the agent's row, else the workspace's, else the system's,
else the catalog default. Turning an override off deletes the row, so that
level falls back to whatever is above it. There is no "copy the default down"
step, which is what lets an operator change a system value and have it reach
every workspace that never chose otherwise.

## A setting that is a ceiling

Most of these are preferences: a temperature, a budget, how much an agent may
read. `approve_new_hosts` is not -- it says an agent may not reach a host nobody
reviewed without somebody's word, and it is a setting for a reason worth
stating, because the alternative looked plausible.

The per-operation approvals in [approvals.md](approvals.md) are declared in the
frontmatter of the skill file documenting an operation, which is right for them:
a skill adding a rule about its own endpoint can only make the platform
stricter. A ceiling cannot work that way. It has to hold for an agent with no
skills at all, and a workspace must not be able to escape it by publishing a
skill that omits a line. So it cascades like everything else here, and the API
turns it into ordinary gates when it prepares a turn.

## Who may override

Part of the catalog, not of the roles. Each setting is either
*operator-only* or *workspace-overridable*, and a workspace-overridable setting may
also be overridden per agent.

- Temperature, reasoning effort, max tool rounds: workspace-overridable.
- Model routing: operator-only. The operator certified a workflow against a
  model and pays for it; a workspace switching models breaks both. A workspace that
  brings its own key gets model choice within what the operator has certified,
  and that is a routing feature (see [routing.md](routing.md)), not a setting.
- Storage retention per scope, when built: operator-only default, workspace may
  shorten.

## What the UI does with it

A settings page reads the catalog and shows each setting's *effective*
value and where it came from: "platform default", "set by this workspace",
"set for this agent". Beside each workspace-overridable setting is an **Override**
checkbox. Off, the value is shown grayed and inherited. On, it becomes
editable and a row is written. The agent editor uses the same component for
agent-level overrides.

The point is that a workspace who does not know what temperature is never sees a
blank number they are expected to invent. They see a value that is already
right and a checkbox they can leave alone. Only the operator has to understand
every setting, and the operator is the one who set the defaults.

## Skills as a level

When skills exist, a skill sits between workspace and agent in the walk and may
pin settings: an invoice skill certified at low temperature and high reasoning
effort declares that, and an agent running the skill gets those values however
the workspace configured itself. A skill's pins are declared in its frontmatter,
because they are facts about the skill; who is billed for running it is not,
and never belongs there.

## What is in it

Temperature, reasoning effort and model calls per turn, moved out of the
agent's free-form `policy` JSON, where they had no defaults above the agent and
no way for an operator to set them once. Storage retention comes when scopes
do. The endpoints are `/v1/settings` (workspace), `/v1/platform/settings`
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
  scoped to a workspace, stored as an agent- or workspace-scope file. It is read
  into prompts, not resolved as a default.

A generic key-value store with a generic editor would be the quick way to
build this and the wrong one: the catalog is what keeps the page honest
about types, defaults and who may touch what. Purpose-built features that
happen to store their "who may override" bit in one place is the intended
layering.
