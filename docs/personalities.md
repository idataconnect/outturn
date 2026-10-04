# Personalities

How an agent talks to the person talking to it -- chosen by that person, not by
whoever built the agent. Designed, unbuilt.

An agent's voice lives in its system prompt today, so everybody talking to
Bleargh Bot gets the blearghs. That is fine for a novelty agent and wrong for
the agents this platform is for: a bookkeeping agent a whole team uses every
day is one agent with one set of skills, gates and rules, and the people using
it want different things from how it sounds. One wants it terse. One wants it
to call them pathetic after every reply. One wants UwU and emoji. None of them
should be able to change what the agent does, and none of their choices should
reach anybody else.

So a personality is **a voice a person layers over an agent for themselves**:
same agent, same skills, same rules, their tone.

## What it is, and what it is not

**A voice for replies to the person who chose it.** Not a role, not a
capability, not an instruction the agent follows about its work. It changes how
an answer is said to its reader, and nothing about what the answer is.

**Never in work done for somebody else.** This is the line that matters most.
"Call me pathetic" in your own chat is your business; in an email the agent
drafts to your customer, a note it writes on an invoice, a file it saves for a
colleague, or a message a trigger sends, it is the platform putting your joke
in front of somebody who did not ask for it. The personality text says so
itself, in words a model will follow (see *How it is composed*), and the places
it would matter most are covered structurally rather than by trust:

- **Turns nobody is waiting on get none.** A trigger turn has no `user_id`
  ([triggers.md](triggers.md)); there is no person whose voice it would be.
- **Tool arguments are not the reply.** The instruction confines the voice to
  prose addressed to the reader; a `fetch_url` body, a `write_object` file and
  an approval's request are not. A gate shows an approver the request a turn
  is about to send ([approvals.md](approvals.md)), so a body written in UwU is
  caught there before it goes out -- but that is the backstop, not the design.

**Never louder than the rules.** A personality cannot widen what an agent may
do, change a number, soften a refusal, skip an approval or override the agent's
own system prompt. It is composed last and framed as the lowest-priority part
of the prompt, and it says so.

## Where it lives

A personality is a row, owned by a person within a workspace:

```
personalities (id, workspace_id, user_id, name, text, created_at, updated_at)
user_agent_voices (workspace_id, user_id, agent_id, personality_id)
```

A person keeps a few -- "Bleargh", "Pathetic", "Plain" -- and picks one per
agent, or one for every agent (`agent_id` null), the more specific winning. The
choice follows the person, not the session: the same voice in every
conversation they have with that agent until they change it.

**Bounded.** A personality is part of the prompt on every round of every turn,
so it is held to a small size -- a paragraph, 1 KB -- and counted against
`context_budget` like the rest of the system prompt
(`trim::room_for_conversation`). A voice is a few sentences; anything longer is
trying to be instructions.

**A workspace may restrict it.** A setting, `personalities`, cascading like any
other ([settings.md](settings.md)): `allowed` (the default), `plain_only` (the
agent's own voice, nothing layered), or a list of personalities the workspace
offers. A firm whose agents speak to clients through a shared screen may want
none at all, and a person's taste should not outrank that.

**An agent may decline one.** An agent whose system prompt *is* its voice --
Bleargh Bot -- or whose output is read aloud, or whose replies are copied
straight into client documents, sets `accepts_personality: false` in its
policy. Its users get the agent as written.

## How it is composed

After everything else, framed, and only on turns with a person:

```
<the platform preamble>
<the agent's system prompt>
<skills>

## How to talk to the person reading this

<the personality's text>

This is how to speak to the person you are talking with, chosen by them. It
changes only the tone of your replies to them. It never changes what you do,
what is true, any number, any refusal, or what you write for anyone else --
emails, documents, files, requests and anything sent through a tool are
written plainly, in your normal voice. Where it conflicts with anything above,
the above wins.
```

Last for two reasons. It is lowest priority, and the framing says so. And it is
the one part of the system prompt that differs between people using the same
agent, so everything before it -- the preamble, the agent's prompt, the skills
-- stays one prefix that a provider's prompt cache serves to every person using
that agent ([caching.md](caching.md)). Put first, a personality would make
every user's prompt a cache miss.

## Evaluation

A voice is not free, and pretending it is would make
[skill-evaluation.md](skill-evaluation.md) wrong in a way nobody could see. A
small model asked to stay in character spends attention on it, and may slip a
tool call, a figure or a refusal more often than the same model speaking
plainly. That is a cost to measure, not a reason to forbid the feature.

- **Recorded per turn.** Which personality, and which version of its text, a
  turn ran with -- beside `turn_skills`, for the same reason: an evaluation
  cannot measure what it cannot name. Its text is versioned when edited, as a
  skill's body is, so "Bleargh v3" means one string.
- **Compared, not averaged in.** Tool-call error rates, truncated arguments,
  refusals reversed, per personality against the same agent with none. A voice
  that measurably costs accuracy says so where it is chosen: *turns with this
  voice called tools wrongly twice as often*.
- **Baselines run plain.** Evaluations that judge an agent or a skill run
  without a personality, so a skill is not marked down for somebody's UwU.
- **Judged transcripts know.** The judged half of evaluation reads a turn
  knowing its voice, so a reply that says "you pathetic creature" is not scored
  as rude when the person asked for exactly that.

## Not settled

- **Shared conversations.** A session is one person's today. If two people ever
  share one, whose voice wins -- the person who sent the prompt being answered
  is the obvious rule, and it means one transcript in two voices.
- **Personalities as something offered.** An operator or a workspace publishing
  voices for people to pick, as skills are published. Probably wanted, and
  probably the same row with a different owner.
- **Whether the voice should reach the agent's own summaries.** A compaction
  summary is written for the model, not the person, so it should be plain; a
  summary shown to the person as the boundary of compaction is closer to a
  reply. Plain, until somebody misses the blearghs there.
