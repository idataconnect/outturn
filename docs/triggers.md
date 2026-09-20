# Triggers

Starting a turn when nobody is typing.

Schedules are being built. Webhooks and email are designed here and not built,
so that the first one does not get in their way.

## Why this matters more than it looks

A turn begins because somebody sent a message. That makes an agent a thing you
visit, and a workspace is not sitting in the UI most of the time -- their day
is spent in the systems the agent is supposed to be helping with.

Everything else a workspace might want an agent to react to has no way in. A
booking arrives and the welcome note should be drafted. It is Monday and last
week's figures should be summarised. A customer replies to a thread and
somebody should decide whether it needs a person. None of that is a message
anyone will type.

## One mechanism, three front doors

All three reduce to the same two steps: put a message in a session, enqueue a
turn against it. That machinery exists and is unchanged -- `ChatTurnPayload`
already says `user_id` is "absent on turns nobody sent -- a schedule, a
webhook", written before anything could produce one.

What differs is what arrives and who it is acting for. So the shared design is
below, and each front door is only the part that is its own.

## Who a triggered turn is acting as

Two labels, not one, because two different questions are being asked.

**The owner** is the person who set the trigger up. Durable, recorded on the
trigger rather than the turn, and the answer to "who is accountable for this
happening". A schedule somebody created at 3am on a Sunday is still theirs at
3am on a Sunday.

**The sender** is who is waiting for the reply, and for a triggered turn there
is nobody: `user_id` stays null. That is what the usage ledger already expects,
and it is what keeps a triggered turn from clearing a stopped session's latch
-- `worker::inhibited` requires "a prompt carrying a real `user_id`", precisely
so an agent cannot restart itself.

Conflating them would break one or the other. Putting the owner in `user_id`
makes a schedule look interactive, lets it lift a latch nobody lifted, and
attributes to a person a turn they were asleep for. Leaving both null loses the
audit trail, and a platform that cannot say who set a thing running is one
nobody should give to tenants.

A triggered turn runs with the agent's own scopes, which are workspace-level
and not the owner's. That is deliberate: an agent should not gain reach because
an admin happened to schedule it.

## Which session

**A new session per firing, for schedules.** A weekly summary is not a
conversation that has been going for a year; it is a fresh piece of work that
happens weekly. One long session would carry a year of history into every
prompt and pay for it on every round.

A shared session is the wrong fix for "the agent should remember last week".
What that wants is something durable and narrow, and the shape of it is not
settled -- an agent writing notes for its future self is a judgement about what
mattered, which is nearer to carry-over than to user-declared memory, and
AGENTS.md is explicit that those must not share a store. It is also, as much as
anything, an evaluation problem: knowing what was worth keeping means knowing
what went wrong without it. Left out of this design on purpose.

**For webhooks the answer is external identity.** A hook that fires per booking
wants one session per booking, so a later event about the same booking lands in
the conversation that already discussed it. That means a mapping from the
sender's id to a session, which the schedule case does not need and should not
be made to carry.

## What runs away, and what notices

A trigger is work nobody is watching, which is exactly the condition under
which a loop runs for a long time before anyone finds out.

Three shapes, and they fail differently:

- **A schedule that fires faster than its turns finish.** The session's serial
  key holds turns in order, so they queue rather than overlap -- but the queue
  grows without bound, and with a new session per firing there is no serial key
  joining them at all.
- **A hook fired a thousand times**, by a misconfigured sender or a malicious
  one.
- **A cycle**: an agent's action causes an event that triggers the agent.

There is no per-workspace fairness in the queue, so one workspace's runaway is
everyone's. That is stated in AGENTS.md as an accepted risk for human traffic,
where a burst is bounded by how fast people type. A trigger is not bounded by
anything.

So each trigger carries a ceiling -- a maximum concurrent and a maximum per
hour -- enforced when the turn is enqueued rather than when it runs, because
refusing early is what keeps a runaway out of the queue instead of merely out
of the runtime.

**Repeated failure is the signal worth surfacing.** A schedule whose turns
fail every time is a broken integration nobody will notice, because nobody was
waiting. It belongs on the dashboard and in notifications, neither of which
exists yet -- see *What this needs that does not exist* below.

## Who reads the reply

A triggered turn produces a reply nobody asked for. The transcript holds it,
which is enough for somebody who goes looking and useless for somebody who
does not.

That is the gap that decides whether triggers are useful. An agent that drafts
a welcome note every morning into a session nobody opens has done nothing.

The answer is notifications, which do not exist. Until they do, a triggered
turn's output is reachable two ways that do: the session is listed like any
other, and an agent that writes its result to `workspace/` scope leaves
something a person will find where they already look. Neither is a substitute.

## What this needs that does not exist

Recorded here because triggers are what make the absence matter, not because
they are triggers' to build.

**Notifications.** Both the runaway alarm and the reply nobody is waiting for
land here. Undesigned: what is notified, to whom, through what -- in-app,
email, webhook out -- and how a workspace says what it wants to hear about. A
dashboard panel is the smallest version and probably the first.

**A dashboard for agent health.** Triggered turns failing repeatedly is the
first thing that needs saying out loud, and `/v1/usage` already carries the per-
turn rows a panel would read.

## Schedules

The first one, and the cheapest, because the queue was built for it.

`jobs.run_after` schedules work into the future, and the claim reads `priority`
before it, so a backlog of scheduled turns cannot put itself in front of
somebody waiting. Both already exist and are already tested.

A schedule is a row: workspace, agent, owner, a cron-like expression, a
timezone, a prompt, whether it is enabled. Firing it means creating a session
and a message, enqueuing a turn at background priority, and writing the next
occurrence.

**The timezone belongs to the schedule.** "Every weekday at 9" means nine where
the person who set it is, and a turn's clock already works this way --
`current-time` takes the zone from the turn rather than the machine. A schedule
in UTC would fire an hour wrong for half the year.

**The next occurrence is computed after the firing, not before.** A schedule
that computed a whole series ahead would drift when it was edited and would
need a sweep to correct it; one that writes its successor as it fires has no
series to correct. It also means a schedule that is disabled stops by simply
not writing another.

**A missed firing does not stack.** A deployment that was down for a day should
not wake to twenty-four hourly turns queued. When the next occurrence is
computed, anything already in the past is skipped to the next one that is not,
and the skip is recorded so somebody can see it happened.

### The prompt

A schedule carries the text the turn begins with, which is the whole of what
distinguishes "summarise last week's bookings" from "check for unpaid
invoices". It is stored as an ordinary message, so the transcript reads the
same as any other conversation and the agent needs no notion of having been
triggered.

What it must not do is pretend a person said it. The message is stored with no
`user_id`, and the UI draws it as what it is: the schedule's own words, not
somebody's.

### The UI

A list of schedules per agent, and an editor. A cron expression is the storage
format and not the interface -- it is precise and nobody reads it correctly --
so the editor should offer the ordinary shapes (every day, every weekday,
weekly on a day, monthly on a date) and a raw expression for what those do not
cover, with the next few firings shown as plain dates in the schedule's own
zone. Showing when it will actually run is what catches a wrong expression
before it is saved rather than after.

## Webhooks

Designed, unbuilt. The shape, so that schedules do not accidentally decide it:

An inbound endpoint per trigger, at an unguessable path, accepting a POST from
outside the platform. That is a different security posture from anything here
today -- every existing endpoint is authenticated, and this one is reachable by
whoever has the URL.

So it needs, at minimum: a shared secret the sender signs with and the platform
verifies, rejection of anything unsigned rather than best-effort acceptance, a
body size ceiling, and the per-trigger rate ceiling above doing real work
rather than being a formality. An unguessable path is not authentication and
must not be treated as any.

The body becomes the turn's prompt, which means untrusted text from outside
reaches a model that has tools. That is prompt injection with a public front
door, and the answer is not to sanitise the body -- it is that a webhook
trigger names the agent it starts, and that agent's scopes and egress rules are
what bound the damage. An agent fed by a public hook should be configured as
though its input were hostile, because it is.

## Email

Designed, unbuilt, and mostly not its own thing: an inbound email provider
(SES, Postmark, and others) delivers by POSTing to an endpoint, so email is the
webhook path plus parsing.

What is genuinely its own: threading, since a reply should land in the session
its predecessor started, which is the external-identity mapping above with a
`Message-ID` as the key; and the fact that anybody can send email to an
address, so the sender is not merely untrusted but unauthenticated in a way a
signed webhook is not. An allowlist of sending addresses is the obvious first
control and is probably enough for a long time.
