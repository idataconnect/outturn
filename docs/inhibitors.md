# Inhibitors

How work is stopped or held, by whom, and what it takes to start again.

## What exists

The storage and the join. `inhibitors` rows in the database, an
`InhibitorStore` that takes, releases and resolves the cascade, and
`inhibitor::decide` -- a plain function over a slice that returns the verdict
and everything that contributed to it.

Two checkpoints consult it. Turn preparation decides whether a runtime is handed
work at all: a `stopped` verdict refuses the turn and latches the session with
what stopped it, the job completes rather than failing, and the runtime is told
only that there is no work -- it is not the tier that decides, so it is not told
why. The gateway decides whether a turn already running may keep going.

Both latch, and both go through `decide` to say why. A stop reported by one and
a stop reported by the other name the same holds in the same words: two holds at
once otherwise have the gateway naming one and preparation naming both, and
whoever reads the stopped session releases the hold they were shown and finds it
still stopped.

The latch is `agent_sessions.stopped_at` and `stopped_reason`, cleared by a
prompt carrying a real `user_id` **whose turn then goes on to run**. Both halves
matter. A person nudging a conversation that is still held does not lift the
latch: it is read, the holds are evaluated, and only a `Proceed` clears it.
Clearing it first and re-taking it when the verdict comes back stopped would
work -- `stop_session` refuses to write where `stopped_at` is already set -- but
only by accident of ordering, and it did not: the clear made the column null, so
the re-take stamped `now()` with the current reason. A conversation held for
three weeks that somebody nudged daily reported being stopped a minute ago, and
`Stopped.at` exists precisely to say otherwise.

Two authorities decide who may hold what. `agents:inhibit` stops one agent and
is held by operators as well as admins -- whoever builds agents is the right
person to stop one misbehaving. `workspaces:inhibit` stops everything and is
admins only, because it halts work its holder may know nothing about. Neither is
folded into the matching `:update`: a credential that exists to halt an org
should not also be able to rename or delete it.

Seeing what is held needs only `agents:read`. Withholding the reason from
somebody watching a silent agent is how "why is nothing happening" becomes a
support ticket.

`POST /v1/workspace/stop` and `POST /v1/agents/{id}/stop` take a hold, both
requiring a reason. `DELETE /v1/inhibitors/{id}` lifts one by the handle taking
it returned -- by id rather than by scope, because several holds can cover the
same work and releasing "the workspace's" would be ambiguous about which. The
UI lists them on the agents page, workspace-wide holds first.

A restarted conversation carries a marker saying why it stopped, placed ahead of
what it explains. Both shapes exist: a message that went unanswered, and a reply
that stops partway.

A hold also cuts a turn already running: the gateway polls for it on the same
tick it polls for cancels, and the reason rides the trailer so the runtime can
say what stopped it rather than looking like a provider that hung up.

Still to come: suspension. Nothing takes a suspended hold -- `POST
/v1/workspace/stop` and `POST /v1/agents/{id}/stop` both write
`Strength::Stopped`, so a suspended row can only be made by hand. The verdict
arm exists and refuses a turn like a stop but without latching: the job
completes, nothing is requeued, and the next turn re-evaluates.

That last part is the gap rather than a detail. A suspension is meant to pause
where the conversation is consistent and resume from there of its own accord --
which is why it needs no marker, there being no fragment to explain. What it
does today is decline, leaving nothing to resume. Parking a turn so it can be
given back to the queue when the hold lifts is the work, and it interacts with
the serial key: a parked turn must not hold its session's queue closed while it
waits.

A reader looking at a paused conversation is told, and by its own event. A
refused turn is declined before a placeholder exists, so without one the message
sits in the transcript with no reply and no indication, and the thread view
cannot read `/v1/inhibitors` to work out why. The event is `chat.held`, carrying
the reason and a `resumable` flag -- true for a suspension, which runs again of
its own accord when the hold lifts, false for a stop, which waits for a person.

Not `chat.error`, for the reason the *Cut mid-turn* section gives below: a
failure and a stop are not the same thing, a failure is retried and a stop is
not, and the browser's error path files the turn under failures and discards the
reply bubble. A reader shown that for a deliberate pause is told the system
broke. And only when a person is waiting -- a turn with no `user_id` is nobody's
pending question, and an announcement for one explains nothing anybody can act
on.

When a session was already latched, what is announced is the reason on the
latch, not the hold that happens to cover it today. Otherwise a session stopped
by one incident and later covered by a second reports the second, while the
transcript, the latch and the next turn's marker all narrate the first.

`decide` is a function rather than anything swappable on purpose: there is one
correct answer and every call path has to get it. A join that could differ
between callers is a kill switch that works in one place and not another.

## Why not a flag

The obvious version is a boolean on the workspace: suspended or not. It fails
the first time two things want it at once. A customer's spend cap trips and sets
it; an operator investigating abuse sets it; the customer tops up and clears it;
the abuse hold is gone and nobody noticed. A single flag cannot say who is
holding it, so releasing is indistinguishable from overriding.

So an inhibitor is a row rather than a bit, and there may be several. Each names
what it inhibits, who took it and why. Work proceeds when none apply. This is
systemd's arrangement and it is the right one for the same reason: the holder is
part of the state, so release is per-holder and the last one out decides.

The other reason is that the same mechanism has to serve cases that look
unrelated. A spend kill switch, an operator stopping a runaway agent, and a turn
waiting for someone to approve a tool call are the same question -- may this
work continue right now -- asked by different parties. Building three mechanisms
means three sets of checkpoints, three ways to display "why is nothing
happening", and three bugs when they overlap.

## The join

Each inhibitor contributes one of two strengths, and the verdict is the
strongest that applies:

    proceed  <  suspended  <  stopped

Zero inhibitors is the empty case of the same rule, not a special path.
`suspended` means the work may continue later. `stopped` means this turn is
over.

**The verdict is derived at each checkpoint, never stored.** A turn suspended
waiting for an approval, resumed when the approval arrives, must re-evaluate
everything -- the workspace's kill switch may have come on while it waited.
Storing "suspended because X" and resuming on X's release would walk straight
through a stop that arrived in between. What is persisted is that the turn is
suspended; which inhibitors contributed is a snapshot for display and audit, not
the authority on whether to go on.

**The UI shows every contributor, not just the winner.** Because `stopped` takes
priority, a turn both stopped by an org switch and waiting on an approval
reports only the stop -- hiding a request somebody is still expected to answer.
The join picks the outcome; a person needs the whole set.

## Levels

Inhibitors cascade the way settings do -- platform, workspace, agent -- and for
the same reason: an operator needs a switch that no workspace can override, and
a workspace needs one that does not require naming every agent. Unlike settings
there is no overriding. A level cannot clear an inhibitor held above it; the set
is the union of every level that applies, and every holder must release its own.

## Where a turn is interrupted

Checking anywhere and resuming anywhere are very different costs. The guest
already names the one place the loop is consistent:

> Injected after the results and before the next model call: the only point in
> the loop where the conversation is consistent and nothing is half-done.

That is the round boundary, and it is where a suspended turn can be picked up
again without inventing state. Tool results are recorded, no call is
outstanding, and the transcript is a conversation rather than a fragment.

A `stopped` verdict is different, and it is checked in two places that answer
different questions.

**The gateway cuts the stream.** It already polls per-session state every 250ms
to carry cancels and steers, so the hold rides that tick: a stop sets the same
flag a person pressing stop sets, and the provider connection closes. This is
what makes the switch worth having. A round boundary can be a whole completion
away, and every token until then is spend past a cap that has already tripped --
which is an arithmetic problem rather than a security one, and applies to a
perfectly well-behaved guest.

**The guest stops at its round boundary.** The gateway's trailer tells it the
turn was cut and why, and it returns at the next boundary keeping what it wrote.
This is the tidy half: tool results recorded, no call outstanding, a transcript
that reads as a conversation.

Neither is a layer of the other. One stops the spend now; the other stops
without making a mess. Whichever arrives first does its job.

Only a stop cuts a stream. A suspended turn is one that will be picked up again,
and cutting it mid-token is how a resumable turn becomes a broken one.

The gateway's check fails open, like everything else it cannot look up: a
database it cannot reach stops nothing rather than stopping everything. That is
the wrong way round for a spend cap and the right way round for an outage. What
it misses, the turn-preparation check catches on the next turn -- unless the
hold is gone by then, which is why a cut turn latches its own session rather
than leaving that to the turn after it.

## The latch

A stop persists until a person restarts it. Flipping the workspace switch back
on does not resume anything, and neither does anything the agent does. This is
what makes it a kill switch rather than a pause with a harsher name.

What clears it is a message carrying a real `user_id`. `agent_messages.user_id`
is null for anything the platform produced and set when an account sent it, so
the agent cannot clear its own latch, a steer it provoked cannot, and neither
can a background job. `role = 'user'` would not do: mid-turn steering already
arrives wearing that role.

The latch is session-scoped -- a column on `agent_sessions` rather than
something inferred from the last turn's outcome. Inferring it makes "stopped"
and "the last turn happened to fail" indistinguishable, and a session that went
quiet for a reason should be able to say so. One workspace switch stopping fifty
conversations needs fifty deliberate restarts, which is the intended cost: a kill
switch that un-kills in bulk is one nobody can reason about afterwards.

## Markers

A stop has to be legible in the conversation, not only in the latch column. The
latch governs whether work may proceed; the marker tells the model what became
of the work that did not. Both are needed and they are not the same requirement.

**There are two situations and they need different words.** Conflating them
produces the failure the marker exists to prevent.

*Stopped before the turn ran.* Nothing started, so there is no partial reply --
the transcript is a prompt followed by silence. What the next turn needs is not
"your reply was cut off" but that a message went unanswered and why. Telling a
model its reply was stopped when it never wrote one invites it to apologise for
a fragment that does not exist.

*Stopped mid-flight.* There is a partial assistant message, and the next turn
replays it. Without an explanation the model reads its own reply trailing off
and either apologises or tries to finish the abandoned thought. So it is told
the reply stops partway, and to carry on from there if that still makes sense.

The transcript says which happened, so nothing has to be recorded to tell them
apart: a turn stopped before it ran leaves the person's message newest, and one
cut mid-flight leaves the agent's. The reply has to have something in it --
a placeholder exists from the moment a turn starts, and an empty one is the
silence case wearing the other shape.

## What the restart answers

Clearing the latch takes a message from a person, and that message starts a
turn. But the prompt that was refused is still sitting there unanswered -- so
somebody who asks a real question, is stopped, and comes back later with
"hello?" would get an answer to "hello?" while the question is never addressed.

The restart turn sees both. This is the batch resume a suspended turn already
needs: `injected()` formats several arrivals as *"mid-turn message N of M;
respond to each of the M in order"*, and an unanswered prompt plus the message
that restarted it is that list with two entries.

## Stopping is lazy

A workspace hold does not latch its sessions when it is taken. Each session
latches as it next tries to run, which means a workspace with fifty
conversations has fifty latches only if all fifty were attempted while the hold
was on.

This is the right trade -- nothing walks a workspace's sessions on a write, and
a conversation nobody touched needs no restarting -- but it makes the earlier
claim more precise. The cost of a kill switch is a deliberate restart per
conversation that *tried to continue*, not per conversation that exists.

A turn cut mid-stream is the exception, and it has to be. It latches on the turn
that was cut rather than the next one, because the hold may be released before
there is a next one -- and a session that was never latched simply carries on,
which is the whole thing a stop is supposed to prevent. That latch write fails
the turn if it fails, rather than being logged and shrugged off: a stop that did
not record itself is a kill switch that silently did not take.

That holds whether the cut turn then ends tidily or falls over. A failure and a
stop look identical from outside -- both end a turn with no reply -- and they
are not the same thing: a failure is retried, and a retry that finds the hold
released runs the work the hold existed to prevent. So the reason travels out of
the runtime on the error path as well as the successful one, and the latch is
written before the failure is reported.

## Input is never blocked

A suspended turn keeps accepting messages. Refusing them is the one place the
agent would suddenly stop listening, and a composer greyed out because the agent
is waiting on a human reads as broken however deliberate it is.

Messages arriving while suspended queue rather than being folded into a running
turn, because while suspended there is no guest running to poll for them. Resume
considers the whole queue at once -- the existing injection already formats
several arrivals as *"mid-turn message N of M; respond to each of the M in
order"*, so this is the built path with a longer list.

Nothing is superseded and nothing is interpreted. An earlier draft had a new
message cancel a pending approval, on the reasoning that "actually, forget it"
should not let a deletion through. It decides consent by reading prose: "hurry
up" or "any luck?" would silently kill a request nobody withdrew. The human
resolves the hold explicitly, and until then agent progress waits while input
does not. That is what a gate is.

## Human in the loop

HITL is an inhibitor of strength `suspended`, held by a pending request and
released when somebody answers it. It is not a tool result. A denial delivered
as a tool result launders a governance decision through the model's context,
where the model can narrate it, misreport it or carry on -- and a gate that only
fires when a tool happens to be called is not a gate.

An approval mints a capability that the retry carries, rather than flipping the
request to approved and hoping the same path is taken. The shape is already in
this codebase twice: an egress rule records the skill whose declaration opened
it, and a turn carries a gateway token minted for that turn alone. A grant with
provenance, narrow in extent, is the pattern.

**What the capability covers is the open question.** A single request, a
predicate over requests, or an authority conferred for a while. It does not
block the work below, because a single-request key is the narrowest case of a
scoped one: if the key carries a scope from the start, widening it later
reshapes nothing.

The hazard in widening is the classic permission-dialog failure -- a person
approves the instance they were shown, not the class it belongs to. Somebody
reading "delete /tmp/build-cache" and clicking approve has consented to that, and
a system that reads it as "file deletion" has invented consent nobody gave. The
test that sorts them: **would a person seeing two different instances of this
permission ever answer differently?** Reaching one host twice is the same act
both times, so approving the host is honest -- which is why the egress model
works. Deleting a different file each time is not.

A category grant, if it exists, is therefore ticked deliberately by the approver
and never inferred from what was asked. And it needs a stated extent -- this
call, this turn, this session, until revoked -- because a key without one is a
standing grant, and a standing grant is an authority that the roles UI cannot
see.

### Who may approve, and of what

Two questions that look like one.

*May this person answer approvals at all* is an ordinary authority, and gets
one: held by whoever a workspace decides, most likely admins and the operators
who run the agents.

*Is the approver entitled to the thing being approved* is the harder one, and
the answer depends on who owns the concept. Where the authority is one of ours,
requiring it is right and its absence is a real escalation: somebody approving a
workspace file write they could not perform themselves has lent the agent an
authority nobody gave them. Where it belongs to a remote system -- "create work
ticket" in somebody's ticketing API -- it is not ours to check. The vocabulary
in `rbac.rs` is fixed and code-defined; a customer's idea of who may dispatch a
technician was never in it and cannot be.

So for a remote call the approval is what it says it is: a person who holds
`approvals:answer` looked at this and said yes, recorded with what they saw.
Modelling more would mean maintaining a mapping from every integration's
operations onto authorities we do not own, to make a guarantee the remote system
never asked for -- it either checks the credential the workspace supplied or it
does not.

Two smaller notes for whoever builds this. A workspace could be given
authorities of its own -- strings meaningless here and meaningful there -- which
is a small schema change and a large conceptual one; worth doing only once
something needs it. And an integration's endpoints reach the world through
egress, which approves a *host*: a workspace that allowed `api.example.com` has
allowed every endpoint on it, which is why a per-request gate is a separate
mechanism rather than an extension of that one.

Tagging endpoints by risk is the right shape applied too early. It is meaningful
once there are endpoints to tag and one can see whether they sort into groups or
each want their own answer; invented beforehand it is a taxonomy fitted to
nothing.

## Rate limits take inhibitors

Everything above is a switch: somebody decides to stop something. What is
missing is a governor -- the thing that catches a runaway nobody is watching, at
three in the morning, before anyone thinks to look. `max_tool_rounds` is the
only guard today and its own description says what it is not: "a runaway guard,
not a budget: what costs money is tokens". It bounds one turn and says nothing
about a thousand.

The case to design for is an invoice-processing agent that should work at a
steady pace, spawning a hundred sessions that each loop and spend ten thousand
dollars in a few minutes. Every session looks reasonable on its own. **So the
bucket is per agent, across all its sessions** -- a per-session or per-turn
limit misses this entirely, because the damage is in the aggregate and each part
of it is unremarkable.

A token bucket rather than a ceiling per hour, because "burst but do not
rampage" is exactly what a refill rate and a capacity encode separately. A fixed
ceiling either blocks legitimate bursts or sits so high it never fires.

**Tokens across all models, and the clunkiness is accepted.** An Opus token and
a Haiku token cost wildly differently, so this is a crude proxy for money -- and
a fine one for a runaway guard, which is what it is. Pricing stays out, for the
reason `docs/usage.md` gives.

**Charged on completion, admitted optimistically.** A call's cost is known only
after it returns, so the bucket is always a little behind and a single enormous
call can overshoot. Against a looping agent spending five figures, one call of
slack is a rounding error, and the alternative -- reserving an estimate and
reconciling -- buys accuracy nobody needs here.

The gateway is where it belongs: the only tier that sees every model call, holds
a database connection, and can refuse before the tokens are spent. It already
polls per-session state on a tick and already cuts streams, so a bucket that
empties mid-stream uses machinery that exists. Note the turn token names a
workspace and a session but no agent, so a per-agent bucket resolves the agent
from the session the way the mid-flight check already does.

**An exhausted bucket takes an inhibitor.** It is a machine holder with a reason
-- "token bucket empty: 2.1M tokens in the last hour" -- and everything below it
is already built: the join, the latch, the panel, the release. That also answers
what a machine-held inhibitor looks like, which was an open question.

What is undecided is whether the hold lifts when the bucket refills or waits for
a person. Auto-release makes it a governor and manual makes it a tripwire; the
likely answer is auto-release *with* a record, so a rampage is visible
afterwards even though work resumed on its own.

## Order of work

**Kill switches first, without HITL.** They are pure `stopped`: no resume, no
capability, no category question. They still exercise every structurally hard
part -- where the checkpoints sit, the join, the latch and what clears it,
truncating a round mid-stream, and making a stop legible to the next turn. Once
that spine is proven, HITL is `suspended` plus resume plus the capability, laid
on machinery that already works.

The other order means debugging suspension and capability scoping at the same
time, against checkpoints that have never fired.

**Notifications after that.** They look independent and are not: until
inhibitors exist there is nothing to notify about, and what the notification
system has to carry is decided by what generates the events.

**The approval *rule* waits for something to point at.** HITL's mechanism --
suspend, hold, resume -- is independent of what triggers it and can be built
whenever. How an integration's endpoint declares that it needs approval is not:
designing that without a single real endpoint in front of you is how a schema
comes out fitting nothing. The OpenAPI work comes first, with no approval
concept in it at all, and the rule is designed afterwards against operations
people actually want gated.

## Not yet

- The capability's extent, above.
- Whether an exhausted rate limit's hold lifts on refill or waits for a person.
- Where a rate limit is configured. The settings cascade is the obvious home,
  but a bucket has two numbers rather than one and the cascade takes scalars.
- Whether a suspended turn can be stopped by timeout, or waits indefinitely.
  Waiting is the honest default and costs nothing while input is unblocked.
- What an inhibitor costs to display when it is held by a machine rather than a
  person -- a spend cap tripping wants to say which cap and at what figure, and
  that is the holder's to supply.
