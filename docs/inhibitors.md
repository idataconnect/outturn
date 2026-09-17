# Inhibitors

How work is stopped or held, by whom, and what it takes to start again.

## What exists

The storage and the join. `inhibitors` rows in the database, an
`InhibitorStore` that takes, releases and resolves the cascade, and
`inhibitor::decide` -- a plain function over a slice that returns the verdict
and everything that contributed to it.

Nothing consults it yet. The checkpoints, the latch and the markers are the next
piece, and the API that lets somebody take a hold comes with them.

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

A `stopped` verdict is different: it does not have to resume, so it can take
effect anywhere -- mid-stream, mid-tool-call -- at the cost of leaving a partial
reply behind. That cost is worth paying. A kill switch that waits politely for a
round boundary is not a kill switch, and the thing being stopped is often
precisely a turn that will not stop on its own.

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

A stopped turn leaves a partial assistant message in the transcript. The next
turn replays it, and without an explanation the model reads its own reply
trailing off mid-sentence -- and either apologises for it or tries to finish the
abandoned thought.

So a stop is legible in the conversation, not only in the latch column: the
projection emits what happened, so the next turn sees that the reply was stopped
by the workspace's kill switch rather than an unexplained fragment. The latch
governs whether work may proceed; the marker tells the model what became of it.
Both are needed and they are not the same requirement.

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

## Not yet

- The capability's extent, above.
- Whether a suspended turn can be stopped by timeout, or waits indefinitely.
  Waiting is the honest default and costs nothing while input is unblocked.
- What an inhibitor costs to display when it is held by a machine rather than a
  person -- a spend cap tripping wants to say which cap and at what figure, and
  that is the holder's to supply.
