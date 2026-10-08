# History access

An agent reading its own past conversations. Designed, unbuilt, and off by
default when it is built.

## Why

A schedule's examples are the obvious ones -- "summarize yesterday's bookings",
"note anything unusual from overnight" -- and an agent today has no way to do
them from what it said and heard. It sees the conversation it is in and
nothing else. Each scheduled run starts a fresh session, so a morning digest
has no morning to digest.

Often it does not need one. Hollowbrook's "Morning arrivals" schedule asks the
business's own system, through its bookings API, and that is the better
answer whenever the facts live there: the system of record is right where a
transcript is only what somebody said about it. And an agent that wants to
remember something between runs can keep notes in its `agent/` scope
([storage.md](storage.md)), which it decides the contents of. History access
is for what neither covers -- what people actually asked, what the agent told
them, where a conversation went wrong -- and that is narrower than it first
looks, which is part of why it starts switched off.

## Why it is off until somebody turns it on

Conversations are the most sensitive thing the platform holds, and a tool that
reads them is a tool that can repeat them to somebody else. Four ways that
goes wrong, each already guarded against somewhere this would otherwise undo:

- **Between a workspace's customers.** A session carries an `account`
  ([usage.md](usage.md#the-account-label)) because one workspace serves many
  of its own customers. An agent reading history across accounts can quote
  one customer's booking to another.
- **Around per-agent narrowing.** Somebody scoped away from an agent cannot
  read its sessions ([authorities.md](authorities.md#narrowing-an-authority-to-some-agents)).
  An agent that reads them and answers that person is a way round the scope.
- **Through injected text.** An agent fed by a webhook reads words a stranger
  chose ([triggers.md](triggers.md#what-the-agent-is-asked)). With history,
  "list everything discussed this week" plus any host the agent may reach is
  a working exfiltration, and no prompt wording prevents it.
- **Into durable places.** A summary written to `workspace/`, or into a PDF,
  outlives the conversation it came from and reaches whoever can read where it
  was put.

None of these is a reason nobody may have it. Each is a reason somebody turns
it on deliberately, for an agent they have thought about, rather than finding
every agent could do it all along.

## The setting

One more entry in the settings catalog ([settings.md](settings.md)), beside
the two that gate storage and shaped like them: one ordered choice rather than
flags that can combine strangely.

| Value | Label | Reads |
|---|---|---|
| `none` | No access | Nothing. The default |
| `same_account` | This account's conversations | This agent's sessions with the same account as the turn |
| `all_accounts` | All of this agent's conversations | This agent's sessions, whatever their account |

Workspace-overridable like the file settings, so an operator can hold it at
`none` for the platform and a workspace can raise it only where the operator
allows, and an agent's own row is where it is normally set.

`same_account` matches the account exactly, absent included: a turn with no
account reads sessions with no account, never everybody's. That is the setting
a customer-facing agent wants. `all_accounts` is for an agent that works for
the business rather than for one of its customers -- a staff digest -- and is
labeled for what it does, because that is the value that crosses the line the
account exists to draw.

## Whose conversations, within that

The setting says how far an agent may look. It never widens what the person
behind the turn could open themselves:

- **A person's turn** reads only sessions that person may read -- their own,
  and other people's with this agent if they hold `sessions:read` for it. The
  narrowing in authorities.md applies untouched, because it is that person's
  visibility being consulted, not the agent's.
- **A triggered turn** has no sender ([triggers.md](triggers.md#who-a-triggered-turn-is-acting-as))
  and reads as its trigger's **owner**, the person accountable for it running.
  A schedule somebody set up can see what they could see, and stops seeing it
  when they lose the right to. A trigger with no owner -- its owner removed --
  reads nothing.
- **Never another agent's conversations**, at any setting. An agent's id is
  what scopes its files and its sessions; reading across agents is a
  workspace-level act, and would want a design of its own and a reason nobody
  has given yet.
- **Never the session the turn is in**, which the agent already has, and
  never a session deleted, whatever was cached about it.

## The tools

Two, offered only when the setting is above `none`, so an agent that may not
read history is not told it could:

- `list_conversations` -- this agent's sessions the turn may see, newest
  first, filtered by a time range: id, title, account, when it started and
  last moved, and how many messages. Paged and capped, like every list.
- `read_conversation` -- one session's messages, as a ranged read the way
  `read_object` is: the person's and the agent's words, with tool calls named
  but their results left out. A result is often a whole API response, and the
  reply that followed it says what mattered.

What comes back is quoted text from other people, and the tool says so in the
shape of its result. That is not a defense against injection and is not
described as one. The defense is the list above: what a turn may read is
bounded before the model is asked anything, and an agent fed by a hook should
be one whose setting is `none` or `same_account`.

## The record

Every conversation a turn read is recorded against that turn, as
`turn_skills` records the skills a reply was given. An audit can then answer
"which conversations did this reply draw on", and so can a person: the
transcript already shows the tool calls, and the record is what survives the
transcript's own trimming. Read-only throughout -- no tool here changes a
session, a title or an account.

## Not settled

- **Whether compaction summaries are the better source.** A long session has
  a summary already written for the model ([compaction.md](compaction.md));
  reading that first would be cheaper and closer to what a digest wants. It is
  also a model's account of a conversation rather than the conversation.
- **A warning where the combination is dangerous.** An agent with a webhook
  and `all_accounts` is the exfiltration case above. Refusing the combination
  is defensible; saying so on the agent's page may be enough.
- **Retention.** When a workspace's sessions are pruned or a person's data is
  removed, what an agent wrote down from them in `agent/` or `workspace/` is
  not. That is true of a person's notes too, and is a policy question before
  it is this feature's.
