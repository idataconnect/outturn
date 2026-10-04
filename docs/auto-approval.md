# Auto-approval

Letting a workspace stop being asked about something it has stopped needing to
be asked about. Designed, unbuilt.

[approvals.md](approvals.md) is the gate: an operation's frontmatter declares
that it needs somebody's word, the gateway refuses the request until a grant
exists, and a person answers from the queue. That is right while nobody knows
whether the agent does the job well. It is wrong once somebody does: an
administrator who has approved two hundred spam moves unchanged is not
reviewing the two hundred and first, they are clicking. A gate everyone clicks
through is worse than no gate, because it still reads as one.

So a workspace administrator can decide that some gated requests no longer need
a person -- for one agent, for some operations, within limits -- and the
platform records every request that went through that way as having done so.

## What this is not

**Not removing the gate.** The gate stays declared, committed and enforced. An
auto-approval is a grant the policy issued instead of a person, and it is
recorded as one: *approved by policy 7, set by Alice on 3 October*. Deleting the
rule from the skill would lose the record, and the record is most of what
approvals are for. It is also the half of approvals.md's own warning worth
repeating: a gate nobody commits to is a gate nobody enforces.

**Not the agent's judgement.** A policy may only test facts about the request:
which operation, and the values of fields the gate already names. Never the
agent's account of why -- "auto-approve when it has classified the message as
spam" trusts exactly the judgement the gate exists to check, and an injected
instruction that persuades the agent a message is spam persuades the policy
too. "Auto-approve moving a message to the spam folder" tests the request. The
first is refused; the [admin agent](admin-agent.md) offers the second in its
place, and says when there is no second.

**Not standing grants.** approvals.md left "until revoked" undesigned because it
is an authority rather than an approval, and the place for one is where a list
of who holds what already exists. A policy is that list for this purpose: rows a
workspace can read, each naming who set it.

## Who declares what

**The operator declares risk, and a ceiling.** Two keys beside `approval` in an
operation's frontmatter:

```yaml
approval:
  requires: charge
  matches: POST /charges
  binds: [payment_account_id, booking_id, amount_pence]
  risk: high
  auto: allowed
  numbers: [amount_pence]
```

- **`risk`** -- `low`, `medium` or `high`. A summary for people choosing in
  bulk, and nothing more: it is not checked against anything and grants nothing
  by itself. Absent reads as `high`, so an operation nobody thought about is not
  swept into "everything medium or lower".
- **`auto`** -- `allowed` or `never`. Whether a workspace may auto-approve this
  at all. `never` is for the operations where a person's word is the product: a
  charge above some figure is still a charge, and an operator selling a booking
  platform may want a person on every refund whatever a client prefers. Absent
  reads as `allowed`, because an operator who wants the opposite says so once per
  operation, and the default the other way would make every new skill's gates
  unwaivable until somebody revisited them.
- **`numbers`** -- which of `binds` are numbers a policy may bound. Everything
  in `binds` may be tested for equality; only what is listed here may be
  compared. The unit is the operation's prose, which the admin agent reads
  anyway, rather than a type system in frontmatter -- the precise part is only
  "this field is a number and may be bounded".

[inhibitors.md](inhibitors.md) called tagging by risk "the right shape applied
too early ... a taxonomy fitted to nothing". It is arriving because there is now
something to fit it to: the OpenAPI wizard produces hundreds of operations, and
choosing among them one at a time is the thing nobody will do. It is still only a
selector. Nothing reads `risk` except a policy choosing what it covers.

For a skill made by the wizard these keys come from the operation's annotations
([openapi-wizard.md](openapi-wizard.md#a-derivation-not-an-output)), so they
survive the specification being updated. For a skill written by hand they are
written by hand, the way `approval` already is.

**A workspace's own skills** declare their own gates, and their own ceilings, by
the same keys. There is no operator to defer to there.

**The operator caps it across the deployment** with a setting,
`auto_approve_max_risk` -- `none`, `low`, `medium` or `high` -- cascading
operator to workspace like every other ([settings.md](settings.md)), and
narrowing only: a workspace may lower it, never raise it. Default `none` at the
operator, so a deployment opts in.

## The policy

Rows, per workspace:

```
approval_policies (
  id, workspace_id,
  agent_id,              -- null: every agent in the workspace
  selector,              -- what it covers, below
  conditions jsonb,      -- tests on bound fields, below
  mode,                  -- 'trial' or 'on'
  created_by, created_at, expires_at, revoked_at
)
```

A selector is one of:

- **up to a risk** -- "everything `medium` or lower";
- **an act** -- every gate whose `requires` is `move_to_spam`, across skills;
- **one operation** -- a gate by its skill and `matches`.

Most specific wins when two apply: an operation over an act over a risk, and a
policy naming an agent over one that does not. A policy may also *exclude*,
which is how "everything medium or lower, except refunds" is two rows rather
than a negation inside one. An exclusion beats an inclusion at the same
specificity.

Conditions are a conjunction of tests on fields in the gate's `binds`:

| test | on |
|---|---|
| `=`, `≠`, one of a list | any bound field |
| `<`, `≤`, `>`, `≥` | a field listed in `numbers` |

Nothing else. No disjunction, no expressions, no fields outside `binds`. A
policy is the thing deciding whether money moves without a person, and the
language it is written in should be small enough that what it permits can be
read off the row. Disjunction is two policies. A field outside `binds` is one
the operator did not consider significant, and keying a permission on it is
the whole-body digest approvals.md rejected, from the other side.

A policy that names a field absent from the request does not match. Absent is
not zero, and not the empty string.

## Where it is decided

**At the gateway, from the token, like every other grant.** The gateway is the
tier that has the request body, and the only tier a compromised runtime cannot
influence. So the API resolves the policies that apply to a turn's agent when
it prepares the turn, intersects them with each gate's ceiling and the operator
cap, and signs what survives into the turn token beside the gates and grants it
already carries. A gate the policy covers becomes, in the token, a gate with a
conditional grant attached.

When a request matches a gate, the gateway asks in order: is there a grant from
a person (the `call` and `unit` extents approvals.md built); failing that, does a
committed policy match this request's bound fields. Only then does it refuse.

What the token commits to is the policy as it stood when the turn started -- the
same rule as egress, where removing a host does not reach a running turn. That
is the wrong way round for a policy being *withdrawn*, which should reach a turn
already running, so revoking one is also a hold: the API takes a suspended hold
on every session of the agents it covered, which parks each at its next round
boundary and re-prepares it against the policies as they now stand. A turn that
was relying on the policy asks a person on its next gated call. The hold is
lifted by the re-preparation itself, so nobody has to answer it.

## The record

**Written by the gateway, before the request goes out.** An auto-approved
request is one no person saw, so the record of it cannot depend on the runtime
relaying it -- a compromised runtime would simply not -- and cannot be written
afterwards, because a crash between sending and recording would leave a charge
nobody can account for. So the gateway writes the row, then sends:

```
auto_approvals (id, workspace_id, policy_id, session_id, job_id,
                requires, method, host, path, bound jsonb, at)
```

`bound` is the bound fields' values, which is what the policy tested and what a
person reading the record later needs. Not the body.

That needs the gateway's database, which is optional. A gateway without one
does not auto-approve: the policy is ignored and the request is refused as if it
did not exist, so the turn asks a person. Falling back to asking is the safe
direction, and it is the only one here -- the alternative is a request nobody
approved and nobody recorded.

A record written for a request that then failed is still a record: it says the
policy let a request out, which is true. Whether the request landed is
[idempotency.md](idempotency.md)'s question, and this does not answer it.

The record is also what the evidence below is built from, and what
[skill-evaluation.md](skill-evaluation.md) reads alongside a person's approvals:
two kinds of yes, kept apart.

## Trial first

A policy starts in `trial`. The gate still asks a person, exactly as before;
what changes is that the queue item says *this would have been auto-approved by
policy 7*, and the answer is recorded against that. After a week the
administrator can see how often the policy would have said yes to something a
person said no to -- which is the number that decides whether turning it on is
safe, and the only one that can, because it is the policy's own disagreement
with people on real requests.

Trial is evaluated by the API when it raises the approval (`api::gated` already
re-derives the turn's gates from the refused request), so it needs nothing from
the gateway and cannot let anything through. A policy goes from `trial` to `on`
by somebody choosing to, never by a threshold being crossed: a policy that
switched itself on would be the platform deciding what a person delegated.

## Evidence

What somebody sees when choosing, and what the admin agent shows when it
proposes a policy:

- **The record so far**, for the gates the policy would cover: how many times
  each fired for this agent, how many a person approved, how many they declined.
  `approval_grants` and the action queue already hold all of it.
- **The policy against that history**: of the last fifty requests, these
  forty-one would have gone through and these nine would still have asked --
  listed, so the nine can be read. A policy that would have approved something a
  person declined says so first.
- **Trial results**, once there are some.

No threshold is enforced. An administrator may switch on a policy for an
operation that has fired twice; the page says it has fired twice. A minimum
would be a number nobody can choose, and the cost of choosing it wrong is
refusing a decision that was the administrator's to make.

## Who may set one

A new authority, `approvals:delegate`, in the `admin` template, and never alone:
setting a policy also requires holding `approvals:answer`, because a policy is
answering in advance, and nobody may delegate an answer they could not give.
Both are checked when the policy is written and recorded on it; neither is
re-checked on each use, for the same reason a grant is not.

Setting one is never itself auto-approved and never done by an agent alone.
The admin agent may propose a policy; a person confirms it, through an endpoint
the agent's token cannot reach ([admin-agent.md](admin-agent.md)). Otherwise an
instruction injected into something the admin agent reads could widen what is
waved through, which is the one outcome this whole design must not permit.

## Not settled

- **When the person who set a policy leaves.** Their policies could lapse, be
  kept and flagged, or move to whoever holds `approvals:delegate`. Lapsing is
  the safe direction and the disruptive one.
- **Expiry by default.** A policy with no `expires_at` is standing for ever.
  Whether new ones should default to ninety days and be renewed from the
  evidence page is worth deciding with a real deployment's habits in view.
- **Host gates.** `approve_new_hosts` turns reaching a new host into a gate with
  no `binds`. Whether a policy may cover those -- "Bleargh Bot may reach any host
  the workspace allowed without asking" -- is the setting being turned off for
  one agent, and is probably better said as exactly that.
- **A policy suspending itself.** If something later marks an auto-approved
  request as wrong -- a refund, a flag from evaluation -- the policy that let it
  through could return to `trial`. What counts as "marked wrong" is not defined
  anywhere yet.
