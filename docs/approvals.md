# Approvals

When an agent's action needs a person's word first, who is asked, and what
their yes covers.

Designed, partly built. `docs/action-queue.md` has the queue that shows a
person what is waiting on them, and `docs/inhibitors.md` has the hold that
stops the work. This is the part between them: what declares that an operation
needs approving, what an approval is worth once given, and how far it reaches.

## Why the declaration lives in the skill

An approval rule is prose about an operation: *charging a card needs a manager*.
The place that already holds prose about operations is the skill that documents
them, one file per operation, and [skill-bundles.md](skill-bundles.md) made
those files into something a rule can safely live in:

- **Versioned and immutable.** A version is a body and its files published
  together, so what the rule said on a date is recorded rather than inferred.
  Before that split the files were overwritten in place and only the body had a
  history -- a governance rule stored that way is one nobody can prove the
  wording of.
- **Written through the skill's own authorities.** `SkillsWrite`, not
  `StorageWorkspaceWrite`. Prose that governs behaviour must not be editable
  through the path meant for reference spreadsheets.
- **Read-only to the agent**, through the `skill/<slug>/` scope, resolved
  against the version the turn bound. A guest cannot edit the rule that governs
  it, and a turn that started before an edit is governed by what it started
  with.

The alternative homes are worse in specific ways. An egress rule is per host,
and a charge and a room lookup are the same host -- host granularity cannot
express "charges need approval, reads do not". A setting in the cascade is a
scalar per key, and this is a rule per operation. A column on the agent would
say nothing about *which* of its calls.

## The frontmatter

YAML at the top of an operation's file, fenced by `---`, before the prose. A
file with none is an operation that needs no approval, which is nearly all of
them -- so the common case stays a markdown file with no preamble, and nobody
documenting a read has to know this exists.

```markdown
---
approval:
  requires: charge
  covers: booking
  identified_by: booking_id
---

# charge_payment_account

Charge the payment account attached to a booking...
```

Parsed by `api::skill::frontmatter`, by hand rather than with a YAML crate. The
contract is three scalar keys under one heading and is deliberately that narrow:
a full reader would accept anchors, aliases, multi-document streams and nested
collections, none of which mean anything here and every one of which is a shape
somebody eventually writes and expects to work.

What it refuses is worth knowing, because each refusal is a rule that would
otherwise be half-applied. A block with no `requires` names no act. A `covers`
with no `identified_by` offers a unit nothing can be keyed on, so a grant for it
would quietly become a grant for every call -- the widening this document is
most careful about. An unknown key *under* `approval` is refused, since under a
block this tier acts on it may be the difference between gated and not; an
unknown key *beside* it is left alone, so a file written for a later version
still reads as prose. And `approval:` written as a scalar is refused rather than
skipped as an unknown key, because skipping it would leave an operation meant to
be gated ungated.

`k8s/components/hollowbrook/skill/charge_payment_account.md` is the worked
example, and the parser's tests read it off disk -- so an edit that breaks the
declaration fails the suite rather than a cluster.

Three keys, and each earns its place.

### `requires` -- what is being asked, in a word

The name of the act, used in the queue row and in the record afterwards. A
person deciding sees "charge", not the tool name and not the URL.

Deliberately not free prose. It is compared: two operations declaring the same
`requires` are the same act for the purposes of one grant, and the queue groups
by it. Prose would make that comparison a string match on a sentence.

### `covers` -- the unit one yes may span

Absent, an approval covers **this call and its retries, and nothing else**. That
is the default and it is the one that needs no judgement from the approver: they
were shown a charge and they approved that charge.

Present, it *offers* a wider extent -- here, every declared `charge` against one
booking. The offer is not the grant. **The approver ticks it, and it is never
inferred from what was asked**, because the hazard is the classic
permission-dialog failure that [inhibitors.md](inhibitors.md) states:

> a person approves the instance they were shown, not the class it belongs to

The test for whether an offer is honest is the one that document gives: *would a
person seeing two different instances of this permission ever answer
differently?* Reaching one host twice is the same act both times, which is why
the egress model can approve a host. Charging £40 and charging £4,000 are not,
which is why `covers` bounds an act on a *unit* and never an entity.

So `covers: booking` means "the charging of this booking" -- one act, however
many calls it takes to complete. It does not mean "anything touching this
booking": a refund declares a different `requires`, so a charge grant does not
cover it, however tempting the grouping looks on screen.

### `identified_by` -- which argument names the unit

Required when `covers` is present, meaningless without it. The name of a field
in the request the grant is keyed on, so a grant for booking `b-8812` covers
that booking and not the next one.

Read from the request the gateway is about to make, not from anything the guest
asserts separately. A guest that could name its own unit could name the one that
was already approved.

## What a yes is worth

**An approval mints a capability the retry carries**, rather than flipping the
request to approved and hoping the same path is taken -- the shape
[inhibitors.md](inhibitors.md) asks for, and the shape this codebase already
uses twice: an egress rule records the skill whose declaration opened it, and a
turn carries a gateway token minted for that turn alone.

A row rather than a token, for two reasons. The wait crosses a park that may
last days and a pod that may not survive it, so a signed capability would need
an expiry nobody can pick honestly. And a grant has to be auditable after the
fact: a standing grant nobody can list is an authority the roles UI cannot see.

### Extent, and why the ceiling is the turn

Every grant states how far it reaches, because one that does not is standing.

- **This call** -- the default, keyed as `docs/idempotency.md` derives keys:
  `(session, turn, call-ordinal)`. Dedupes the retry of one call and nothing
  else. This is what stops a resumed turn asking again, which is the whole
  reason the capability exists.
- **This unit, this turn** -- what a ticked `covers` grants. It dies with the
  turn, so nothing accumulates and nothing granted at nine reaches a call at
  five. That bound is deliberate: "this session" reads as convenient and is the
  version that goes wrong quietly, because the person who approved is no longer
  watching by the time it is used.

Wider extents -- this session, until revoked -- are left undesigned. They are
authorities rather than approvals, and the place to add one is the roles model,
where a list of who holds what already exists.

## Who may answer

Two questions that look like one, per
[inhibitors.md](inhibitors.md#who-may-approve-and-of-what).

*May this person answer approvals at all* is an ordinary authority,
`approvals:answer`, held by whoever a workspace decides. It goes to `admin` in
the templates and deliberately not to `operator`: an operator builds and runs
the agents that raise these, and the default should not be the one where the
person who wrote the agent signs off its charges. A workspace that wants that
can add it, which is the point of roles being the workspace's own.

*Is the approver entitled to the thing approved* depends on who owns the
concept. Where the authority is ours, requiring it is right and its absence is a
real escalation. Where it belongs to a remote system -- charging a card in
somebody's payment API -- it is not ours to check: the vocabulary in `rbac.rs`
is fixed and code-defined, and a customer's idea of who may take money was
never in it. So for a remote call the approval is what it says it is: a person
holding `approvals:answer` looked at this and said yes, recorded with what they
saw.

Which is why the queue targets a **role** rather than a person, and resolves
membership when the queue is read -- see
[action-queue.md](action-queue.md). Who may approve a charge is a question about
the workspace's own organisation, and it changes without the pending request
changing.

## How the gate fires

Two halves, and only the first is being built now.

**An explicit request.** Built. `POST /v1/approvals` raises one: a suspended
hold scoped to the session, and a queue item naming it. `POST
/v1/approvals/{id}/answer` answers it, and an approval releases the hold, which
gives the parked turn back.

Three things in there are worth knowing before changing them. Raising takes
`agents:inhibit` and answering takes `approvals:answer`, because asking and
answering are opposite acts and sharing an authority would make anybody who may
approve a payment able to park any conversation. An answer finds its item
through the caller's *own queue*, so an approval somebody was not asked about is
not theirs to answer however senior they are -- and an item that is no longer
open is told apart from one that was never theirs, because "already decided" is
what two people answering at once produces. A decline leaves the hold on: the
turn stays parked, which is honest, since nothing has changed about whether the
work may proceed.

**The automatic gate**, later. The gateway is the only tier that sees every
outbound request, holds the credential, and can refuse before anything is spent;
it is also the tier a compromised runtime cannot influence. What it cannot do is
know which *operation* a request is: it receives a method, a URL and a body, and
the file that declared the rule lives in the API.

So the rule has to reach it the way egress rules already do -- committed by the
API into the turn token and verified against that commitment, so a guest cannot
strip it and "could not verify" means refused. The API reads the bound skills'
frontmatter when it prepares a turn, commits the matchers, and the gateway
matches the request it is about to make. That is a change to
`src/egress/commit.rs` and the token, which is why it waits until the mechanism
above is proven.

**Not the guest asking.** A `request_approval` host call would be a gate that
fires only when the guest chooses to ask, which is the same fault
[inhibitors.md](inhibitors.md) names in delivering a denial as a tool result. A
guest may additionally ask -- "this looks unusual, check with a human" -- as a
judgement above the mandatory gate. What must not happen is the mandatory gate
depending on it.

## Not yet

- The automatic gate, above: the commitment, the matcher, and what the gateway
  does with a request it must hold.
- What the queue row shows for a charge. The payload carries what every target
  may see and no more (see `NewItem` in `api::actions`), which for a charge is
  the figure and the unit rather than the conversation that led to it.
- Whether a declined approval is distinguishable from an expired one in the
  transcript. Both leave the turn unable to proceed; only one of them was a
  decision.
- Tagging operations by risk, which [inhibitors.md](inhibitors.md) already
  places: the right shape applied too early, worth revisiting once there are
  enough declared operations to see whether they sort into groups.
