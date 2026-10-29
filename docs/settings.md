# Settings

How defaults cascade from the operator to workspaces to agents, and who may
change what. Built: the catalog is `crates/api/src/settings/mod.rs`, the rows are
`setting_overrides`, and the walk is `SettingsStore::resolve`, called once per
turn in `prepare_turn`. It holds nine settings, listed under
[What is in it](#what-is-in-it).

Settings that only mean something to a model -- `temperature`,
`reasoning_effort`, `context_budget` -- are moving to a cascade keyed by model
as well as level; see [models.md](models.md#settings-belong-to-a-model).

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
else the catalog default. Three settings are about a model -- temperature,
effort and the conversation's size -- and may also be chosen for one model at
any level; how that cascades, and why the gateway resolves two of them, is in
[models.md](models.md#settings-belong-to-a-model). An agent made from a template has one level more,
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

Part of the catalog, not of the roles. Each setting is *operator-only*,
*workspace-overridable* -- which may also be overridden per agent -- or an
*operator ceiling*.

An operator ceiling is for a feature a deployment may need to forbid: the
operator's value bounds what a workspace may choose, so a workspace may be
stricter and never looser. An operator who turns it off turns it off
everywhere, and a workspace asking to turn it back on is refused rather than
saved and ignored; one who leaves it on leaves each workspace free to turn it
off for itself. A row a workspace wrote before the operator turned it off is
bounded when read, so it does not get through either. Stricter means earlier
in the choice's options, which are listed strictest first. It stops at the
workspace: a ceiling is about the people using the product, so an agent's page
does not offer it and a template cannot fix it. Dictation is one;
`auto_approve_max_risk` ([auto-approval.md](auto-approval.md)) is the other.

A deployment may also hand an operator-only setting to workspaces, where who
should set it depends on how the deployment is sold: `Setting::delegated_by`
names the operator-only setting that does it and the value that means yes.
While it says yes, the setting is workspace-overridable -- per workspace,
never per agent. The storage quota is the one (`max_stored_bytes`, handed
over by `stored_bytes_set_by`; see [storage-quota.md](storage-quota.md)). The
operator also sets any setting for one workspace, including one it keeps to
itself, at `PUT /v1/workspaces/{id}/settings/{key}`.

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
| `max_written_bytes` | File writes per turn | 1 GiB | 0 to 1 TiB, in bytes; zero is no limit |
| `max_stored_bytes` | Storage | 0, no limit | 0 to 1 PiB, in bytes; per workspace only; operator-only unless handed over |
| `stored_bytes_set_by` | Who sets storage | `operator` | `operator`, `workspace`; operator-only |
| `context_budget` | Conversation size sent to the model | 400,000 bytes, or two bytes a token of the model's window where less | 1,000 to 100,000,000 bytes; at most two bytes a token of the window |
| `session_naming` | When to name a conversation | `after_first_turn` | `after_message`, `after_first_turn`, `after_second_turn` |
| `dictation` | Dictation | `on` | `off`, `on`; an operator ceiling, per workspace only |
| `auto_approve_max_risk` | Auto-approval | `none` | `none`, `low`, `medium`, `high`; an operator ceiling, per workspace only |

## Each setting

What each one is for, and when to change it. The help button beside a
setting in the app opens its section here.

### Temperature {#temperature}

How much the model varies its wording from one answer to the next. Lower
suits an agent that follows a procedure and should answer the same way each
time; higher suits drafting and brainstorming. Left unset, the provider's own
default applies. Some models take no temperature at all, and the setting is
skipped for them.

### Thinking before answering {#reasoning_effort}

How much a model that can reason does so before it answers. Higher answers
hard questions better and takes longer and costs more; lower is quicker and
cheaper. Off makes most models noticeably worse at multi-step work, and a
model that cannot stop thinking is asked for its lowest level instead. Each model offers its own levels, and a
level it does not offer is moved to the nearest one it does. Off with prompt
repetition is explained [below](#prompt-repetition).

### Agent files {#agent_file_access}

What an agent may do in `agent/`, the files it keeps from one conversation to
the next: notes, a running list, something it built up over time. Read only
lets it use files somebody put there without changing them. See
[storage.md](storage.md) for the three scopes.

### Workspace files {#workspace_file_access}

The same for `workspace/`, the files every agent in the workspace shares --
price lists, policies, templates. Read only is the default, so one agent
cannot change what the others rely on.

### Approve new hosts {#approve_new_hosts}

Whether an agent reaching a public website it has not used before in this
conversation needs a person to say yes first. Hosts a skill declares are
already allowed and are not asked about. Ask for approval suits an agent
that browses on somebody's behalf; reach freely suits one that only calls the
services its skills name. See [egress.md](egress.md).

### Model calls per turn {#max_tool_rounds}

How many times one turn may call the model. Each step an agent takes -- a
tool call and its result -- is a call, so a turn that does a lot of work
needs more. It stops an agent that is stuck repeating itself. Zero removes
the limit, for agents that run long jobs on purpose.

### File writes per turn {#max_written_bytes}

How many bytes one turn may write to files. It stops an agent stuck writing
in a loop from filling the store. A write that would go past it is refused
before anything is stored. Raise it for agents that produce large files on
purpose, such as exports; zero removes the limit.

### Storage {#max_stored_bytes}

How many bytes of files a workspace may hold, session files and the text read
out of documents included. A workspace past it can still read, list and
delete its files, but not write or upload; uploads are refused before
anything is stored, and an agent is told the storage is full. Counted as files
are written and deleted, and corrected weekly from what the store holds; no
quota applies to a workspace until it has first been counted. Set by the
operator, for every workspace or for one, unless *Who sets storage* hands it
to each workspace's admins. See [storage-quota.md](storage-quota.md).

### Who sets storage {#stored_bytes_set_by}

Whether each workspace's storage quota is set by the operator -- by hand, or
from whatever sells the plan -- or by the workspace's own admins, for a
deployment that sells storage by use. Operator-only.

### Conversation size sent to the model {#context_budget}

How much of a conversation is sent to the model on each turn, in bytes. A
long conversation eventually outgrows any model, and when it reaches this
size the oldest tool results are dropped first, then the conversation is
summarized ([compaction.md](compaction.md)).

Left unset it follows each model: 400,000 bytes, or two bytes per token of
the model's context window where that is less. That leaves room for the
reply and for tool results that arrive during the turn, which this does not
count.

It is in bytes because a token is counted differently by every provider, and
the conversation has to be sized before it is known which model will answer.
When a chain has several models, the conversation is sized for the smallest
window among them.

Set it yourself when:

- a model's context window is not in the catalog -- a local model you added
  by hand, say. Use about two bytes per token of the window the server was
  started with: a 32,768-token window is about 65,000 bytes.
- conversations are reaching the limit and costing more than you want. A
  smaller budget sends less on every turn.

A value larger than a model's window allows is ignored for that model.

### When to name a conversation {#session_naming}

When an untitled conversation is given a title. See
[session naming](#session-naming) below for the three choices.

### Dictation {#dictation}

A microphone button in the message box, using the browser's speech
recognition. Most browsers send the audio to the browser's maker to
transcribe. An operator can turn it off for a workspace; see
[who may override](#who-may-override).

### Auto-approval {#auto_approve_max_risk}

The highest risk an auto-approval policy in this workspace may cover. None
means every request that needs approval goes to a person. An operator sets
the ceiling per workspace; the workspace writes its policies under it. See
[auto-approval.md](auto-approval.md).

The endpoints are `/v1/settings` (workspace), `/v1/platform/settings`
(operator) and `/v1/agents/{id}/settings` (agent), each returning every
setting with its effective value, where it came from, what this level would
inherit, and whether this level has its own row. PUT sets a row, DELETE
removes it.

## Session naming

A conversation nobody has titled is named by a model, as a background job
queued when a turn finishes (`crates/api/src/naming.rs`). The setting decides when,
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
while the job waited is kept. With no chain for naming -- its own, or
assistant's, which it borrows -- sessions go unnamed whatever this says.

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
in `crates/api/src/worker/projection.rs`.

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
