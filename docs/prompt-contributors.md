# Prompt contributors

Everything that shapes what a model is given on a turn, other than the
conversation itself, as one kind of thing with one set of rules -- so that a
voice, an eagerly loaded tool, the time a conversation began and a skill's chosen operations
can each be added, owned, measured and eventually published without each
inventing its own way in. Designed; the first piece, composing the system
prompt once per conversation, is built.

## Why one idea

Several things already write into what a turn sends, and each has its own
unwritten rules:

- the platform preamble (`skill::platform_preamble`);
- the agent's system prompt;
- the bodies of its skills, composed in order;
- compaction summaries, framed as summaries (`summarize::framed`);
- and, designed, carry-over, memory and [personalities](personalities.md).

Each of those answers the same questions differently or not at all: who may
add it, what it may override, how much of the budget it may spend, whether it
is the same for everybody on that agent or different per person, which turns
it reaches, and how anybody later knows it was there. [compaction.md](compaction.md) already found
the cost of not asking -- the system prompt was not counted against
`context_budget` until ten long skills overflowed a turn with nothing to say
why. A contributor is the answer to all of those questions, asked once.

And they make agents better in ways nobody has to think about turn by turn. A
weak model that would have spent a round discovering it needs `read_object`
has it already; one that would have guessed the date is told it; a skill whose
three most used operations are loaded up front costs one round less on the
calls that matter. Those are worth offering as things a workspace switches on,
and eventually as things somebody else publishes.

## What a contributor is

A declaration, not code. A contributor says what it adds and where, from a
fixed vocabulary the platform executes -- so a contributor from a catalog is
something the platform can reason about, show a person, and refuse, rather
than something it runs.

Three kinds of contribution:

- **Text.** Words added to the system prompt: a voice, a convention ("money is
  in pence"), a stance ("challenge premises; do not call a design good when it
  is merely fine").
- **Eager loads.** What a guest would otherwise ask for on its first rounds:
  tools passed to `load_tools` before the turn starts, a skill's manifest and
  chosen operations' files read before the model sees the prompt. The same
  content the agent would have fetched, without the round trip it costs to
  fetch it.
- **Eager runs.** A read-only tool run when the conversation starts, its call
  and result put into the conversation as if the agent had made them: the time
  the conversation began is `get_current_time` run eagerly, not a sentence in
  the system prompt.
- **System messages.** A short note from the platform put into the
  conversation before a turn, when a condition the contributor declares holds:
  *it has been four hours since your last turn*, *it is now Tuesday*, *a skill
  you were given was updated since this conversation began*. The other half of
  the time question -- the time a conversation began is a snapshot; that time
  has moved on is news -- and the half that never touches the system prompt.

## When a contribution is made

**Once per conversation, and again only at compaction.** The system prompt is
composed when a conversation starts and then left alone, message after
message, until compaction rebuilds the context -- which is already the moment
the prefix changes, since a summary replaces what came before it. Between
those two moments nothing a contributor says is re-asked: no contribution is
recomputed per message, no eager run repeats per turn, and the prefix a
provider caches is the same on every round of every turn of the conversation.

Built: `session_prompts` keeps a conversation's prompt, the model it names and
the skills it was composed from (`worker::kept_prompt`). A turn sends the kept
one; a compaction, or a different model, composes it again. The skill versions
each turn recorded in `turn_skills` are the kept ones, so a conversation still
on an older version says so. Gates are the exception, read from live versions
every turn: a tightened gate reaches a running conversation at once, and its
prompt may describe a call the gate now refuses, which the refusal explains.
What follows from it, deliberately:

- **An edit reaches a conversation at its next compaction**, not its next
  message. A skill published, an agent's prompt edited, a person's voice
  changed: a new conversation has it at once, a running one when it next
  compacts. The record per turn says which versions a turn actually had, so a
  conversation running on yesterday's skill is visible rather than mysterious.
  A change that must reach running conversations now -- a gate tightened, a
  skill withdrawn for being wrong -- is not a prompt edit; gates are enforced at
  the gateway from the version a turn committed to, and withdrawing is a hold.
- **Eager runs are snapshots.** The time a conversation started is the time it
  was told, said as *the time when this conversation began* so it is not read as
  the time now. That time has since moved on is a system message's job, not a
  recomposition's.
- **System messages are the one thing that happens between.** Before a turn,
  a contributor's condition is checked -- time elapsed, a date crossed, a
  version changed -- and when it holds, a short note goes into the
  conversation, after the cached prefix, recorded and budgeted like the rest.
  The platform already writes such notes, each invented on its own: a retried
  turn is told its last attempt was cut off, a resumed turn is told what its
  approval came to, a prompt is repeated. Those are system messages in all but
  name, and move onto this hook as each is next touched. The conditions are
  designed when the first is built; the hook is recorded here so that they are
  not each invented again.
- **Compaction recomposes all of it**, from the contributors as they stand
  then, and the summary is written knowing the new system prompt, so nothing
  the old one said is lost silently between them.

## Where a contribution goes

The other decision that matters, and the reason the time a conversation began
is better as an eager run than as text. A provider's prompt cache keys on the
prefix of a request ([caching.md](caching.md)); anything that differs moves the
point where the cache stops hitting to wherever it differs, and everything
after is paid in full. Composed once per conversation, a time in the system
prompt would still make every conversation's system prompt unique -- no two
conversations on the same agent could share a cached prefix. As a tool result
at the start of the conversation, it sits after the part every conversation on
that agent shares.

So every contributor names a **placement**, and placements are ordered by how
widely they are shared:

| placement | same for | where | examples |
|---|---|---|---|
| `platform` | every agent | system prompt, first | the preamble |
| `agent` | every conversation with this agent | system prompt | the agent's prompt, skills, a stance |
| `person` | one person's conversations with it | system prompt, last | a voice |
| `conversation` | one conversation | the conversation's opening, before the first prompt | the start time, eager loads |
| `message` | one turn | the conversation, before the turn's prompt | system messages |

Earlier placements are more stable and more widely shared, so they are cached
for more people and for longer. A contributor may not choose a placement wider
than what it varies by: something that differs per person cannot go in
`agent`, or every person's request is a cache miss for everybody else.

## The rules, once

Every contributor, built in or installed, carries the same properties:

- **Owner.** The operator, a workspace, an agent's author, or a person. Who may
  add it, who may remove it, and whose authority installing it takes.
- **Priority.** Where it stands when two contributions disagree. The preamble
  and the agent's prompt outrank everything a person adds, and a contribution
  framed as lower says so in its own text -- a voice cannot soften a refusal
  however it is worded.
- **Budget.** How much of `context_budget` it may spend, counted like the rest
  of the system prompt (`trim::room_for_conversation`). A contributor over its
  budget is not cut silently; it is refused, and the refusal is shown where it
  was installed.
- **Reach.** Which turns it applies to: every turn, only turns with a person,
  only replies to that person. A person's voice has no conversation to reach
  when nobody is waiting; an eager time check does.
- **Recorded per turn.** Which contributors, at which versions, shaped a turn
  -- beside `turn_skills`, generalising it -- so evaluation can measure what
  each one costs and buys, and a transcript can be explained afterwards.
- **Versioned.** A contributor's content is immutable once published, as a
  skill version is, so "the eager-tools contributor v2" names one thing.

## Eager runs are tool calls, with every check a tool call has

An eager run is not a back door. It goes through the guest's own tool
dispatch, the turn's egress commitment and its gates exactly as a call the
model chose would: an eager `fetch_url` to a host the workspace has not
allowed is refused, and one to a gated operation waits for approval like any
other. Read-only tools only, by default -- an eager run happens before a person
has said anything in this conversation, and a contributor that wrote something
whenever one started would be acting on nobody's instruction. The result is recorded in
the transcript as a tool exchange, marked as the contributor's rather than the
model's, so a reader can see the agent did not choose to make it.

## Naming

*Contributor* is the mechanism -- something that contributes to what a turn
sees -- and it is the word for it here and in the code. It is not a product
name. What a catalog offers is a **plugin**: a versioned, installable package
that may hold contributors, skills, and later integrations, which is also the
natural home for what [skill-bundles.md](skill-bundles.md) calls a bundle.
*Extension* is avoided on purpose: [integrations.md](integrations.md) already
uses it for the OAuth integrations a workspace turns on, and one word meaning
two things is the ambiguity [glossary.md](glossary.md) exists to prevent.

## A catalog, later

Contributors as things somebody publishes and a workspace installs, the way
skills are bound -- "eagerly load tools", "load these operations of this skill",
"start every conversation with the time", "a voice", "a reviewer's stance". The
same shape as [skill-bundles.md](skill-bundles.md) and integrations: installing
is a proposal a person approves, what is installed is versioned, and a version
that asks for more -- a wider placement, a bigger budget, a tool that writes --
is a new approval rather than an update.

A contributor from somebody else is untrusted text in the prompt, which is to
say a prompt injection somebody chose to install. The declaration vocabulary is
what keeps that bounded: a catalog contributor can add text at the priority
its owner is allowed and run the read-only tools it declared, and nothing else.
Showing a person exactly what will be added -- the text, the tools, the
placement -- before they install it is the same rule as approving a skill's
hosts.

## What becomes of what exists

Nothing moves for the sake of it. The preamble, the agent's prompt and skills
are contributors in all but name already; they keep their code and gain the
properties above as each is touched. `turn_skills` becomes the first table of
`turn_contributors`. [personalities.md](personalities.md) is the first
contributor designed against this: a `person` placement, lowest priority,
replies only.

A stance -- "hold me to a higher standard" -- is a contributor too, but not a
personality: it is meant to change what the agent produces, so it belongs to
whoever owns the agent's behavior, at `agent` placement, and is evaluated as
part of the agent rather than excused as somebody's taste.

## Not settled

- **Eager loads against a small window.** Loading a skill's operations up front
  spends budget for the whole conversation whether they are used or not. Which to load is
  better chosen from what turns actually call
  ([skill-evaluation.md](skill-evaluation.md)) than guessed, and a contributor
  that loads by usage is the version worth building.
- **Contributors that depend on each other.** "Load these operations" assumes
  the skill is bound. Whether a contributor may declare that, or simply does
  nothing when it is not, is worth deciding with the first one that needs it.
- **Where carry-over and memory land.** Both are per-session or per-person and
  change over time, so `conversation` or `person`; carry-over is what a
  compaction keeps, so it is composed with the summary at exactly the moment
  this design already recomposes.
