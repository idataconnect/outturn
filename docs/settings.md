# Settings

How defaults cascade from the operator to workspaces to agents, and who may
change what. Built: the catalog is `src/api/settings/mod.rs`, the rows are
`setting_overrides`, and the walk is `SettingsStore::resolve`, called once per
turn in `prepare_turn`. It holds eight settings, listed under
[What is in it](#what-is-in-it).

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
else the catalog default. An agent made from a template has one level more,
and it wins over all of these: the values its template fixes
([agent-templates.md](agent-templates.md)). Turning an override off deletes the row, so that
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

- Every setting in the catalog today: temperature, reasoning effort, file
  access for both scopes, new-host approval, model calls per turn, context
  budget and session naming. All workspace-overridable.
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

Temperature, reasoning effort and model calls per turn began here, moved out
of the agent's free-form `policy` JSON, where they had no defaults above the
agent and no way for an operator to set them once. The rest arrived with the
features they govern. Storage retention comes when scopes do. Every setting
below is workspace-overridable, and so overridable per agent too.

| Key | Label | Default | Choices |
|---|---|---|---|
| `temperature` | Temperature | unset, left to the provider | 0 to 2 in steps of 0.1, or unset |
| `reasoning_effort` | Thinking before answering | `low` | `none` (Off), `none_repeat_prompt` (Off, with prompt repetition), `low`, `medium`, `high` |
| `agent_file_access` | Agent files | `read_write` | `none`, `read`, `read_write` |
| `workspace_file_access` | Workspace files | `read` | `none`, `read`, `read_write` |
| `approve_new_hosts` | Approve new hosts | `reach_freely` | `reach_freely` (Reach freely), `approve_new_hosts` (Ask for approval) |
| `max_tool_rounds` | Model calls per turn | 100 | 0 to 10,000; zero is no limit |
| `context_budget` | Conversation size sent to the model | 400,000 bytes | 1,000 to 100,000,000 bytes |
| `session_naming` | When to name a conversation | `after_first_turn` | `after_message`, `after_first_turn`, `after_second_turn` |

Where each is explained in depth:

- `reasoning_effort`: the repetition choice is [below](#prompt-repetition).
- `agent_file_access`, `workspace_file_access`: the `agent/` and `workspace/`
  scopes in [storage.md](storage.md). Session files are always read and
  write, and an agent can always read what it may write.
- `approve_new_hosts`: [above](#a-setting-that-is-a-ceiling), and
  [egress.md](egress.md).
- `max_tool_rounds`: a runaway guard, not a budget; nothing more to it.
- `context_budget`: [compaction.md](compaction.md).
- `session_naming`: [below](#session-naming), since nothing else covers it.

The endpoints are `/v1/settings` (workspace), `/v1/platform/settings`
(operator) and `/v1/agents/{id}/settings` (agent), each returning every
setting with its effective value, where it came from, what this level would
inherit, and whether this level has its own row. PUT sets a row, DELETE
removes it.

## Session naming

A conversation nobody has titled is named by a model, as a background job
queued when a turn finishes (`src/api/naming.rs`). The setting decides when,
and how much of the conversation the model is shown:

- **After the first message** (`after_message`): queued when the first turn
  ends, but the namer is shown only what the person wrote. Cheapest and
  quickest, and as vague as the opening message is.
- **After the first reply** (`after_first_turn`, the default): queued at the
  same point, with the reply in view, so the title says what the exchange
  turned out to be about.
- **After two exchanges** (`after_second_turn`): waits until the session has
  two non-empty replies. Usually the best title, later.

Either way the namer checks again before it writes, so a title a person set
while the job waited is kept. Without a default model (`OUTTURN_DEFAULT_MODEL`)
sessions go unnamed whatever this says.

## Prompt repetition

"Thinking before answering" has one choice that is not a level of effort:
**Off, with prompt repetition** (`none_repeat_prompt`). Thinking is off, and
the turn's prompt is sent twice:

```
<the prompt>
Let me repeat that:
<the prompt>
```

A model reads its prompt one token at a time, each able to look only at what
came before it, so the start of a question is read without knowing how it
ends. A second copy is read with all of the first in view. Google found this
improves answers when a model does not deliberate, and does little when it
does -- thinking re-reads the question anyway ([Leviathan, Kalman and Matias,
2025](https://arxiv.org/abs/2512.14982)). So it is offered as what a model gets
instead of thinking, not alongside it.

Only the prompt this turn answers is repeated. The system prompt and earlier
messages are not: repeating the history would double what each round costs
to restate what the model already answered. It is found by its text rather
than its position, since a turn resumed after an approval does not end on its
prompt; a prompt nobody typed, such as a wake, is sent once. And it changes
only what the model is sent, never the stored conversation -- see `repeated`
in `src/api/worker.rs`.

The cost is the prompt's tokens a second time, on every round of the turn.
After the first round they are part of a cached prefix where the provider
caches ([caching.md](caching.md)).

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
