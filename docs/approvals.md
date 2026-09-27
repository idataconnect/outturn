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
  matches: POST /charges
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

What it refuses is worth knowing, because every refusal is a rule that would
otherwise be half-applied -- and a rule half-applied is an operation the file
says is gated and the platform does not gate. Review found four ways that
happened. `approval :` with a space was unrecognised and its keys skipped, so
the operation was ungated and nothing said so. A wholly indented block was the
same. An unclosed quote made `"charge` the act, which compares unequal to every
grant for `charge`, so the rule existed and matched nothing. And a key given
twice took the last, so a reviewer who read the first had approved something
else. All four are refused now, and there is a test for each named after what it
must not do quietly.

The backstop is the general form of that: a file mentioning `approval` that
produces no rule this understands is refused rather than read as declaring
nothing. Which is also why an empty `approval:` block is an error -- it is a
rule somebody started writing far more often than a deliberate statement of no
rule, and a file needing no approval says so by having no block. A key we do not
know *beside* `approval` is still left alone, so a file written for a later
version of this platform reads as prose here.

`k8s/components/hollowbrook/skill/charge_payment_account.md` is the worked
example, and the parser's tests read it off disk -- so an edit that breaks the
declaration fails the suite rather than a cluster.

## Driven once, against a cluster

Worth recording, because the suite proves the parts and not the path. Against a
live deployment: raising put a `suspended` hold on the session -- the first
thing in this codebase ever to take one -- and an `approval.charge` item in the
asked role's queue with the figure and the account label in its payload. The
badge went to one. Approving released the hold and emptied the badge; answering
again was refused as already resolved. Declining a second request left its hold
on, and the conversation with it.

And the two-person case, which is the one worth being sure of: a clerk holding a
role that was *asked* but does not carry `approvals:answer` saw the request in
their queue and was refused when they tried to answer it. Being asked and being
entitled are separate, and they are separate in the running system rather than
only in the tests.

Four keys, and each earns its place.

### `requires` -- what is being asked, in a word

The name of the act, used in the queue row and in the record afterwards. A
person deciding sees "charge", not the tool name and not the URL.

Deliberately not free prose. It is compared: two operations declaring the same
`requires` are the same act for the purposes of one grant, and the queue groups
by it. Prose would make that comparison a string match on a sentence.

### `matches` -- which request it applies to

A method and a path, as the gateway will see them. `POST /charges`, or `DELETE
/bookings/*` where a trailing star stands for anything below it -- an operation
on `/bookings/{id}` is one rule and not one per booking. A star anywhere else is
refused: a pattern that can match in the middle is one somebody writes `*` into
and gates far more than they meant, and a gate that is too wide refuses work
nobody intended to gate, which reads as the platform being broken rather than as
a rule being wrong.

Declared rather than read out of the prose below it. The body says `fetch_url
with POST http://.../charges` because that is what a model needs; deriving a
security gate by parsing that sentence would make the gate depend on how
somebody phrased a paragraph. The host is not named here either -- it comes from
the skill's own declared hosts, so a gate on `POST /charges` does not gate the
same path on somebody else's API.

Required. A rule with nothing to match is one the gateway cannot apply, and a
file that declares an approval and gates nothing is worse than one that declares
none: it says the operation is gated, and a reader believes it.

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

**An approval should mint a capability the retry carries**, rather than flipping
the request to approved and hoping the same path is taken. This section is
design and not description: nothing mints one today, and the consequence is in
*Not yet* -- an approved turn that resumes into the same gate is refused a
second time -- the shape [inhibitors.md](inhibitors.md) asks for, and the shape
this codebase already uses twice: an egress rule records the skill whose
declaration opened it, and a turn carries a gateway token minted for that turn
alone.

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

## A ceiling, rather than a rule per operation

Everything above is opt-in by whoever documents an operation, and that is right
for it: a skill declaring a rule about its own endpoint can only make the
platform stricter. Some deployments want the other direction -- an agent that
may not reach anywhere new without somebody's word -- and that cannot be a
declaration in a skill, for two reasons. It has to hold for an agent with no
skills at all, and a workspace must not be able to escape it by publishing a
skill that omits a line.

So it is a setting, `approve_new_hosts`, cascading operator to workspace to
agent like every other ([settings.md](settings.md)). Where it is on, the API
turns it into ordinary gates at turn preparation: one per allowed host, every
method, `/*` for the path. Nothing downstream knows the setting exists -- the
commitment, the token claim, the gateway's check, the refusal and the parked
turn are the machinery above, reused whole.

Three things about what it gates are worth stating, because each is a decision
rather than a detail.

**A host, not a request.** The unit a person can honestly approve is a host:
reaching `api.example.com` twice is the same act both times, which is the test
[inhibitors.md](inhibitors.md) sets and the reason the egress model can approve
a host at all. Per request would ask three times for three pages of one site.

**Every method.** A read of an unreviewed host is as much a reach as a write.
The setting is about who the agent talks to rather than what it says to them.

**Not the hosts a skill brought.** A host that arrived because somebody
installed a skill declaring it was consented to already, in an act that named
the skill and the host together -- `POST /v1/skills/{id}/hosts/approve` is that
act, and `egress_rules.from_skill_id` is what records it. Asking again, per
conversation, asks the same question in a worse place. A host added by hand
through `/v1/egress-rules` carries no skill and is not exempt: it says agents
*may* reach that host, not that any particular use of it was reviewed, and that
gap is the one the setting exists to close.

A wildcard is never exempt, however it arrived. Approving a skill's hosts takes
`settings:update`, which is also what turns the ceiling on -- so whoever sets it
can exempt a host from it, and a skill declaring `*.example.com` would otherwise
exempt everything under that domain. No authority boundary is crossed, but the
exemption is meant to be for a host somebody named, and a wildcard names a
class: consenting to a class is exactly the permission-dialog hazard the
`covers` section above is about. A workspace that wants the class exempt can say
so host by host.

What it does not gate is anything nobody allowed. A host with no egress rule is
refused outright and always was; a gate there would be a second answer to a
settled question, and a worse one, since it reads as though approving were
possible.

Off by default. An agent's first call to each new host becomes a stop-and-wait,
which is the point in a deployment that wants it and an obstruction in the rest.

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

**The automatic gate.** Built. The gateway is the only tier that sees every
outbound request, holds the credential, and can refuse before anything is spent;
it is also the tier a compromised runtime cannot influence. What it cannot do is
know which *operation* a request is: it receives a method, a URL and a body, and
the file that declared the rule lives in the API.

So the rule reaches it the way egress rules do. The API resolves a turn's bound
skills, hashes the gates it finds into a root (`egress::gate`) and signs that
into the turn token beside the egress commitment; the runtime relays the set
unchanged; and the gateway checks the set against the root before consulting it.
A runtime that dropped a gate from its copy gets nowhere, because the set no
longer hashes to what the token says.

The asymmetry with egress is the whole design, and it decides which way a
stripped claim fails. An egress rule is a **permission**: a request proves one
and is allowed, so failing to prove means refused and absence is safe. A gate is
an **obligation**: a request matching one is refused until somebody approves,
and absence read as "nothing is gated" would let everything through -- which is
the answer a forger would choose. So a gate commitment is a *second* claim
rather than folded into the first, "nothing gates this turn" is `Gates::none()`
with a root of its own, and a token carrying no gate claim is refused outright
rather than defaulted.

It also means the whole set travels rather than one gate and a proof. A Merkle
proof shows presence; what the gateway has to establish is that a request
matches *none* of the gates, and absence is not something a proof provides.

Two things follow that are worth knowing before changing them. The gate is
declared with a `matches` -- `POST /charges` -- rather than derived from the
prose that tells the model what to call: the body says "fetch_url with POST
http://.../charges" because that is what a model needs, and deriving a security
gate by parsing that sentence would make the gate depend on how somebody phrased
a paragraph. And the gates are recorded when a version is published rather than
computed per turn, because a file's content lives in the object store by hash,
so a turn deriving them would read every bound skill's every file before its
first token. A version is immutable, so what it declares cannot change
afterwards.

What the gateway does on a match is refuse the request, in words the guest can
read, rather than parking the turn: parking is the API's, which owns the job and
the queue, and this tier has a method, a URL and a token. So the money does not
move and get approved afterwards -- which is the ordering that matters.

**Not the guest asking.** A `request_approval` host call would be a gate that
fires only when the guest chooses to ask, which is the same fault
[inhibitors.md](inhibitors.md) names in delivering a denial as a tool result. A
guest may additionally ask -- "this looks unusual, check with a human" -- as a
judgement above the mandatory gate. What must not happen is the mandatory gate
depending on it.

## Not yet

- **The capability**, and with it the loop. An approval releases the hold today
  and nothing more: nothing records what was approved, so a turn that resumes
  into the same gate is refused again. Everything under *What a yes is worth*
  below is design, not description.
- **Raising one from a refusal.** The gateway refuses a gated request and says
  so in words the guest can read; nothing turns that into a held turn and a
  queue item. Somebody calls `POST /v1/approvals` by hand, which means noticing
  first. Those two together are what "the loop closes" would mean, and neither
  is built. - What the queue row shows for a charge. The payload carries what
  every target may see and no more (see `NewItem` in `api::actions`), which for
  a charge is the figure and the unit rather than the conversation that led to
  it. - Whether a declined approval is distinguishable from an expired one in
  the transcript. Both leave the turn unable to proceed; only one of them was a
  decision. - Tagging operations by risk, which [inhibitors.md](inhibitors.md)
  already places: the right shape applied too early, worth revisiting once there
  are enough declared operations to see whether they sort into groups.
