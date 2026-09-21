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

## The fixture this needs

Webhooks are the first thing here where a test against the mounted app is not
enough. Signature verification, refusals and body limits are all testable
in-process, and should be -- but the thing webhooks exist for is a round trip:
an event arrives, an agent works, and something outside the platform is changed
as a result. None of that is exercised by a request built in a test.

So a fixture service, beside `outturn-mockllm` and for the same stated reason:
a test against an in-process shortcut measures the shortcut, and the parts most
likely to break are the ones a shortcut skips.

A small booking system is the shape to build, because it is the worked example
these documents already use. It emits a webhook when a booking is made, exposes
a REST API for availability and reservations, and can be asked what it holds so
a test can assert the agent's work landed. Deployed at zero replicas like the
mock provider, scaled up when something needs it.

Three things earn their keep at once, which is why this is worth building
rather than mocking:

- **Triggers**, end to end: it POSTs a signed delivery and the turn that
  follows is a real turn.
- **Integrations** ([integrations.md](integrations.md)): the agent reaches back
  through `fetch_url`, so the egress rule, the credential and the gateway are
  all in the path rather than assumed.
- **The OpenAPI wizard** ([openapi-wizard.md](openapi-wizard.md)) and
  **evaluation**: it serves its own specification, so the wizard has a real
  document to read and the skill that results has a real service to be judged
  against.

It must be obviously fake from the outside, the way the mock provider's
`x-outturn-mock` header makes a pretend transcript identifiable later by
somebody who does not know it exists.

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

An inbound endpoint per trigger, at an unguessable path, accepting a POST from
outside the platform. That is a different security posture from anything here
today -- every existing endpoint is authenticated, and this one is reachable by
whoever has the URL.

**This is core rather than an add-on**, and the reasoning is worth recording
because it looked like the other way round. A deployment with users already
exposes the API, so one more route is not a new posture. And an agent that
cannot be reached by the systems it is meant to help with can only be polled or
waited on by a person -- which is not the product. What an operator eventually
pays for around webhooks is the operational surround: delivery history, replay
of failures, alerting on a trigger that has stopped receiving. The mechanism
itself has to work in the box or the box does not work.

### Authentication

**A trigger declares one scheme and the platform enforces exactly that one.**
Not a list of things it will accept: a request that fails the declared check is
refused, never retried against a weaker one.

`hmac` is the scheme to prefer. HMAC-SHA256 over the raw body with a
per-trigger secret; the sender sends the digest and a timestamp, the platform
recomputes over the bytes as received, compares in constant time, and refuses a
timestamp outside a few minutes so a captured request cannot be replayed
indefinitely. Over the raw bytes rather than a parsed body, because two JSON
documents that mean the same thing have different bytes, and a signature over a
reserialised body verifies something the sender never sent.

`shared_secret` exists because the alternative is refusing real senders.
Postmark states plainly that it does not support HMAC signing and recommends
HTTP Basic Auth with IP allowlisting instead -- and this document already names
Postmark as how email arrives. A platform that accepts only signatures cannot
receive email, so the choice is not between strong and weak but between working
and not. It carries a per-trigger credential in a header, compared in constant
time.

This is deliberately not "support both and let people pick". The scheme is a
property of the sender rather than a preference, and the difference matters:

- The credential travels on every request rather than a proof of it, so any
  proxy or log that records headers holds it. Anything that reads
  `webhook_triggers` must therefore be as careful with it as with an egress
  credential.
- It does not bind the body, so a request that was captured can be replayed
  with a different payload.

So a trigger using it should say so where somebody will see it, and the
operator's mitigations are the sender's own: restrict by source address where
the sender publishes its ranges, and rotate the secret on a schedule. Neither
is the platform's to enforce, and both are worth telling somebody about at the
moment they choose the scheme.

What neither scheme permits is an unauthenticated request. An unguessable path
is not authentication and must not be treated as any: a URL leaks into logs,
browser history, screenshots and support tickets, and a secret that travels in
the path is a secret sent to everything in between.

### What the agent is asked

**The trigger owns a prompt template and the body fills it.** Not the raw body
as the prompt: that makes the entire instruction attacker-controlled text, with
nothing saying what it is or what to do with it.

A template puts the workspace's own framing around untrusted content -- "A
booking notification arrived. Summarise it and check availability: {{body}}" --
so the model reads the payload as data inside an instruction rather than as the
instruction. That is not a defence against prompt injection and must not be
described as one. It is the difference between a model that has been told what
it is looking at and one that has not.

The real bound is elsewhere, and it is the same one as everywhere else here: a
webhook trigger names the agent it starts, and that agent's scopes and egress
rules are what a compromised payload can reach. An agent fed by a public hook
should be configured as though its input were hostile, because it is.

### Which session

**A new session per delivery**, as for schedules. Each event is independent and
the simplest thing is right for a first version.

The alternative -- keying deliveries by a field in the body so events about one
booking land in one conversation -- is the thing a real deployment will ask
for, and it is deliberately not built. It needs a mapping from external
identity to session, a decision about when a thread is finished, and a story
for two deliveries racing into one session. A nullable key column added later
costs less than guessing at those now.

### Replay within the window is permitted

A delivery signed five minutes ago is accepted five minutes ago, and nothing
records which signatures have been seen -- so the same captured request can be
sent again, as many times as the ceiling allows, until its timestamp ages out.
Each replay is a new session and a new billable turn.

Said plainly because the earlier wording was not: "refuses a timestamp outside
a few minutes so a captured request cannot be replayed indefinitely" is true
and reads as a replay defence, which it is not. It bounds how long replay is
possible and does not prevent it.

For a trigger whose agent acts on the world -- the booking case these documents
use -- that is duplicate work, which is what
[idempotency.md](idempotency.md) is about. Until something records seen
signatures, a trigger's prompt should be written so that acting twice is
survivable, and that is the operator's job rather than the platform's.

`shared_secret` is worse and the difference is worth stating: it binds no body,
so a captured request can be replayed with a payload of the attacker's choosing
until the secret is rotated.

### What is still to settle

**The concurrency half of the ceiling.** The section above specifies "a maximum
concurrent and a maximum per hour" and only the second exists. With a new
session per delivery there is no serial key joining them, so a trigger allowing
sixty an hour against turns that take three minutes runs them genuinely
concurrently -- which is the first runaway shape this document describes,
unbounded by the ceiling that was supposed to bound it.

**The ceiling for schedules.** Unbuilt there too. Less urgent, because a
schedule fires only as often as its own expression says, where a hook fires as
often as whoever holds the URL chooses.

**Per-agent narrowing on trigger creation.** Creating a trigger is a way to
make an agent run turns, and `sessions:create` is narrowed per agent
(`user_agent_scopes`) while `agents:update` deliberately is not. So somebody
narrowed away from an agent can still cause it to run by giving it a schedule
or a hook. Both endpoints have this; neither checks `require_for_agent` the way
`sessions::create_session` does.

## Email

Designed, unbuilt, and mostly not its own thing: an inbound email provider
(SES, Postmark, and others) delivers by POSTing to an endpoint, so email is the
webhook path plus parsing.

It is also why the webhook path has two authentication schemes rather than one.
Postmark does not sign its deliveries at all -- its documentation says so and
recommends Basic Auth with IP allowlisting instead -- so an HMAC-only platform
could not receive email through it. Checking that before building was worth
more than the assumption it replaced.

What is genuinely its own: threading, since a reply should land in the session
its predecessor started, which is the external-identity mapping above with a
`Message-ID` as the key; and the fact that anybody can send email to an
address, so the sender is not merely untrusted but unauthenticated in a way a
signed webhook is not. An allowlist of sending addresses is the obvious first
control and is probably enough for a long time.
