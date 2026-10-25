# Auto-approval

Letting a workspace stop being asked about something it has stopped needing to
be asked about. Built; *As built*, at the end, says where and how, and where
it differs from what follows.

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

**Not the agent's judgment.** A policy may only test facts about the request:
which operation, and the values of fields the gate already names. Never the
agent's account of why -- "auto-approve when it has classified the message as
spam" trusts exactly the judgment the gate exists to check, and an injected
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
  free: [description]
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

  It holds for agents that bind the skill declaring it, and only those. Gates
  come from a turn's bound skills, so an agent given a workspace's own skill
  documenting the same endpoint, with a gate saying `auto: allowed` -- or no gate
  at all -- is governed by that one. A workspace that may write skills can already
  leave an operation ungated, so this is not a new hole, but it means `never` is
  a statement about the operator's skill, not about the operator's API. Making it
  hold for the API is attaching the operator's gates to the integration -- the
  host and the credential the operator bound -- so any turn reaching that host
  with that credential carries them; see *Not settled*.
- **`numbers`** -- which of `binds` are numbers a policy may bound. Everything
  in `binds` may be tested for equality; only what is listed here may be
  compared. The unit is the operation's prose, which the admin agent reads
  anyway, rather than a type system in frontmatter -- the precise part is only
  "this field is a number and may be bounded".
- **`free`** -- the top-level body fields that carry nothing worth approving: a
  note, a client reference. Required with `auto: allowed`, because a policy
  tests only `binds` and `binds` was chosen for another job -- what a person's
  yes is keyed on. A field in neither list is one nobody has said is harmless:
  an email operation whose `binds` names the recipient but not `bcc` would
  otherwise let a policy wave through a copy to anyone. So with `auto: allowed`,
  a request carrying any top-level field outside `binds` and `free` is not
  covered by any policy, and asks a person.

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

A policy may also *exclude*, which is how "everything medium or lower, except
refunds" is two rows rather than a negation inside one. **Any exclusion that
applies wins, however broad** -- deny overrides -- so no ordering question can
let a narrower inclusion slip past a wider exclusion. Specificity only chooses
among inclusions, for their conditions and caps: an operation over an act over a
risk first, and then a policy naming an agent over one that does not.

A risk or act selector reaches gates that did not exist when it was set -- a
skill published later, declaring `risk: low`. Those are covered in `trial` for
that policy until somebody holding `approvals:delegate` accepts them, and the
action queue says they are waiting. Otherwise whoever publishes skills decides
what an existing policy waves through, which is a delegation nobody made.

Conditions are a conjunction of tests on fields in the gate's `binds`:

| test | on |
|---|---|
| `=`, `≠`, one of a list | any bound field |
| `<`, `≤`, `>`, `≥` | a field listed in `numbers` |

Nothing else. No disjunction, no expressions, no fields outside `binds`. A
policy is the thing deciding whether money moves without a person, and the
language it is written in should be small enough that what it permits can be
read off the row. Disjunction is two policies.

What each test means is fixed here, because every obvious reading fails open:

- **A comparison matches only a JSON number**, never a string that looks like
  one, compared exactly as an integer when the bound is an integer, and only
  within the 64-bit range. A string, a float against an integer bound, `1e400`,
  anything else: the test fails, and the request asks a person.
- **A bounded field has a lower bound too.** `≤ 5000` alone admits `-500000`,
  which is a refund dressed as a small charge. A field in `numbers` is bounded
  below at zero unless the policy says otherwise.
- **`≠` and "not one of" only beside a positive test** on the same field. On
  their own they cover every value nobody thought of.
- **Absent does not match.** Not zero, not the empty string, not null.

**A policy covers a gate only if the gate binds every field the policy tests**,
and lists in `numbers` every field it compares. A risk or act selector spans
gates that bind different fields; one that does not bind `amount_pence` is not
covered by a policy that tests it, rather than covered with the test skipped.

**And a policy carries caps.** A condition is per request, so `amount_pence ≤
5000` reads as a limit and bounds nothing: four hundred charges of £49.99 each
satisfy it. So every policy has a count per turn and a count per day, required,
and may have a sum over one `numbers` field per day. Over a cap, the request is
not covered and asks a person.

## Risk from the amount

The backend is built; see *As built* below for where, and what differs from
this section. It changes what a risk is: from a fixed label on an
operation to one worked out from the operation and what a request asks for.

A charge is not one risk. Nobody wants to approve a five-dollar charge, and
everybody wants to approve a ten-thousand-dollar one, and today the only way to
say so is a condition on each policy -- `amount_pence ≤ 5000` -- written per
workspace, per operation, in the field's own units. Every policy restates the
same judgment about money, and a workspace with ten money operations writes it
ten times. The judgment belongs in one place, and the policy should only say
how far it trusts the agent: *auto-approve up to `low`*.

### Who sets what

- **The operator, with the skill.** An operation declares its **base risk** as
  now, and, where it moves money, which bound field is the **amount** and in
  what **currency**, with **default bands** and an optional **floor**:

  ```yaml
  approval:
    requires: charge
    matches: POST /charges
    binds: [payment_account_id, booking_id, amount_pence]
    risk: high
    numbers: [amount_pence]
    amount: { field: amount_pence, currency: GBP, minor_units: 2 }
    bands: { low: 5000, medium: 20000 }   # below each, that risk; above, `risk`
    floor: { high: 500000 }               # from here, high whatever a workspace says
  ```

  The amount field must be in `numbers`, so every rule about comparing numbers
  above applies to it unchanged.
  Where a request names its own currency, `currency_field` says which bound
  field: `amount: { field: amount, currency: GBP, minor_units: 2,
  currency_field: currency }`. A request whose value there is anything but
  exactly `GBP` carries an amount nobody can trust, and takes the base risk.
- **The workspace.** Its own bands for an operation, or for every operation in
  one currency, starting from the operator's defaults -- moved either way, but
  never past the floor. A workspace that thinks £500 is small for its business
  may say so; one that tries to call £10,000 low is held at the floor.
- **The policy.** A risk selector -- *up to `low`*, for an agent -- plus its
  caps and expiry, as now. The amount conditions stay for the case they suit,
  one operation with a rule of its own, but are no longer how the common case is
  written.

### What a request's risk is

At the gateway, from bands committed in the turn token beside the policies, so
the agent cannot argue its way into a lower band:

1. An operation with no `amount` has its base risk. A delete is high whatever
   any band says.
2. Otherwise read the amount as the comparisons above do: a JSON number, an
   integer within range, never a string, at least zero. Anything else is the
   base risk, which is the conservative reading of an amount nobody can trust.
3. The lowest level whose band the amount is below; above every band, the base
   risk; at or above the floor, the floor's level.

Bands are upper bounds, one per level, rising with the level, so they cover
every amount with no gap between "under 200" and "1,000 or more". They are per
currency, never converted: a band in pounds says nothing about a charge in
dollars, which takes the operation's base risk.

**The daily caps still matter most.** A per-request risk cannot see four
hundred charges of £49, each of them `low`. A risk selector's policy keeps its
required counts, and gains a daily total per currency across everything it
covers -- *up to £500 a day of `low`* -- which is what actually bounds an agent
that splits a large payment into small ones.

### The same answer routes the question

A computed risk can also say who is asked. `low` goes through where a policy
covers it; `medium` asks anyone holding `approvals:answer`; `high` asks the
roles a workspace names for it -- a manager, say. The queue already targets
roles, so this is a choice of targets by risk rather than a new mechanism, and
it means one mapping answers both whether a person is needed and which one.

### What the person sees

- **Money as money.** With the amount field and currency declared, every screen
  says "£50.00", never `amount_pence ≤ 5000`.
- **One line for the common case.** "Charges under **£50** go through
  automatically, up to **£500** a day" -- a band, a daily total, the agent and
  an expiry -- with the full editor still there for anything else.
- **An offer drawn from what was approved.** The prompt's link after a few
  approvals of one kind becomes specific: "12 charges approved this month, all
  under £40 -- approve charges under £50 automatically?", the figure prefilled.

### Where the wizard comes in

Suggesting, for the operator to confirm, as it suggests an identity: a base
risk per operation (reads low and ungated; creates and updates medium; deletes
and anything named for money or irreversibility -- pay, charge, refund,
transfer, cancel, delete, send -- high and gated), the amount field from its
name (`amount`, `total`, `price`, a `_cents` or `_pence` suffix) and the
currency where the specification says, and bands from nothing but a sensible
default the operator edits. A model may offer a second opinion on operations a
heuristic misreads -- an `execute` that is really a delete -- but never sets a
risk unconfirmed, since a risk decides what goes out without a person.

### What it replaces, and what it leaves

- The `auto_approve_max_risk` ceiling stays the operator's coarse switch: how
  far auto-approval may reach at all. Bands decide where a request falls
  beneath it.
- Policies already written with amount conditions keep working; nothing about
  conditions changes.
- *Settled:* bands are per workspace, not per agent; a workspace may move a
  band either way from the operator's default, but never past the floor.

## Where it is decided

**At the gateway, from the token, like every other grant.** The gateway is the
tier that has the request body, and the only tier a compromised runtime cannot
influence. So the API resolves the policies that apply to a turn's agent when
it prepares the turn, intersects them with each gate's ceiling and the operator
cap, and signs what survives into the turn token beside the gates and grants it
already carries. A gate the policy covers becomes, in the token, a gate with a
conditional grant attached.

A request goes out only when **every** gate covering it is satisfied, each by a
person's grant (the `call` and `unit` extents approvals.md built) or by a policy
committed against that gate -- against the gate's own leaf, not its `requires`
word, so a policy for one skill's `charge` says nothing about another's. One
gate satisfied says nothing about another: the gateway asked only the first
covering gate until this design's review found that a grant for a host's `/*`
gate let a `/refunds` request past its own.

**What the gateway tests is what it sends.** It parses the body to read the bound
fields, and forwards the original bytes -- so a body with `amount_pence` twice
can satisfy the policy with the first and charge the second, if the remote API
reads JSON differently. For a request a policy admits, the gateway forwards the
body as it parsed it, re-serialized, with `Content-Type: application/json`, and
refuses: a query string on a gated path, which the policy never saw; and a
top-level key that differs from a bound field only by case.

What the token commits to is the policy as it stood when the turn started --
the same rule as egress, where removing a host does not reach a running turn.
That is the wrong way round for a policy being *withdrawn*, and a hold is not the
answer: a hold reaches a runtime only if the runtime listens, and a compromised
one does not. So the token is not the last word. **The record below is the
check**: the gateway writes it with an insert that only succeeds if the policy
is still `on`, unrevoked and unexpired, and within its caps, all in one
statement. No row, or no database, means the request is refused and asks a
person. That is a deliberate exception to this tier deciding from the token
alone, and it is what makes revoking a policy take effect on the next request,
whoever is running the turn.

A token trade-in (`POST /v1/work/{job_id}/token`) does not carry a revoked or
expired policy into the new token: the runtime sends the gates it holds, the API
checks them against the presented token and drops every policy that is no longer
on at the version committed, and the runtime is handed the smaller set with the
token that commits to it. Nothing is added on a trade-in, so an edited policy is
dropped rather than committed again on its new terms; it covers the gate again
from the next turn.

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

- **Operator gates that follow the credential.** `auto: never` binds agents
  binding the operator's skill. Attaching an operator's gates to its integration
  instead -- carried by any turn whose rule uses the operator's credential for
  that host -- would make it bind the API. It wants
  [integrations.md](integrations.md)'s notion of an integration to exist first.
- **When the person who set a policy leaves.** *Chosen: they lapse.* A policy
  applies only while whoever set it still holds a role in the workspace, checked
  in the resolution and again in the gateway's insert; the page shows it as
  lapsed. It is the disruptive direction, and the safe one: nobody's word is
  left standing in for a person who is gone. Moving them to a successor is a new
  policy, set by that person.
- **Expiry by default.** *Chosen: every policy expires.* `expires_at` is
  required in the table, defaults to ninety days from now when a write leaves it
  out, and may be at most a year away. Renewing is an edit, made from the page
  that shows the evidence.
- **Host gates.** *Chosen: not covered.* A gate binding nothing -- every host
  gate -- is covered by no policy, in the resolution and in the gateway alike.
  Turning `approve_new_hosts` off for one agent is a setting, and should be
  said as one.
- **A policy suspending itself.** If something later marks an auto-approved
  request as wrong -- a refund, a flag from evaluation -- the policy that let it
  through could return to `trial`. What counts as "marked wrong" is not defined
  anywhere yet.

## As built

- **Declarations.** `risk`, `auto`, `numbers` and `free` are read in
  `api::skill::frontmatter` and stored beside each gate in
  `skill_version_gates` (migration 0056). `numbers` must be bound and `free`
  must not be. `free` is required where `auto: allowed` is written out; where
  `auto` is absent, `free` reads as empty -- so a version published before these
  keys existed is coverable only for a request carrying nothing but its bound
  fields, rather than every such skill being refused at publish.
- **The cap.** `auto_approve_max_risk` is an operator ceiling in the settings
  catalog, `none` by default.
- **The rows.** `approval_policies` and `auto_approvals`, migration 0056.
  Policies are never deleted; revoking is a column. Each row records who set it,
  who last changed it, who revoked it, and a `version` every edit moves on.
- **Who.** `approvals:delegate`, in the `admin` template and backfilled as
  `approvals:answer` was. Writes need it and `approvals:answer` together, and a
  browser session -- a token carrying a refresh family -- so no agent's token
  can write one. Reading needs either.
- **The API.** `GET/POST /v1/approval-policies`, `GET/PUT
  /v1/approval-policies/{id}`, and `POST .../revoke`, `.../expire` (ends it now,
  renewable by an edit) and `.../accept` (covers the gates published since).
  `GET .../evidence` returns the record so far, trial results, and for an act or
  operation selector the last fifty approvals of what it covers, each with
  whether the policy would have let it out -- those it would have approved that
  a person declined first. Replayed as the gateway would decide now
  (`auto_approval::api::replay`): the tests, then for a money operation the
  risk its amount works out to under the bands as they stand, held to the
  tighter of the policy's bound and the deployment's, then the day's total
  counted over the day before each from what the replay let out. One stopped by
  its amount says why: `£290.00 is medium risk`, or the total it would pass. `GET /v1/approvals/answered?requires=` is the count the prompt reads.
- **Resolution** is `api::auto_approval::choose`, called from `prepare_turn`
  through `for_turn`. Deny overrides; specificity chooses among inclusions;
  *two equally specific inclusions choose neither*, and a person is asked,
  rather than one winning by an order nobody set. A risk or act selector covers
  a gate published after the policy was last accepted only in trial.
- **In the token.** A `PolicyGrant` per covered gate, committed beside the gates
  and grants under its own tagged leaf, against the gate's own leaf
  (`Gate::key`), carrying its tests, the fields a request may carry (`binds` and
  `free`), the caps, the policy's version and the turn's job id.
- **At the gateway.** `egress::policy::PolicyGrant::admits` and `record`.
  Comparisons only on whole JSON numbers in range, a floor of zero, absent never
  matching, `≠` and "not one of" only beside `=` or "one of" on the same field.
  The record's insert is the check: the policy row is locked, and the insert
  succeeds only while it is on, unrevoked, unexpired, at the committed version,
  set by somebody still in the workspace, and within the tighter of the
  committed caps and its own. "Per day" is the last twenty-four hours. No
  database, or no row, and the request is refused as gated, so a person is
  asked. What goes out is the parsed body, serialized again, as JSON.
- **Trial** is marked by `api::gated` on the queue item as
  `would_auto_approve`, by the same test the gateway applies; the evidence
  counts the answers to those items.
- **On a trade-in**, `api::auto_approval::still_standing` keeps each
  committed `PolicyGrant` whose policy is still `on` at its version, and
  `work::refresh_token` re-signs over the gates without the rest, returning the
  new set (`protocol::work::TokenRefreshed::gates`) only when it changed. A gate
  set that does not hash to the presented token's root is refused; a runtime
  that sends none has its commitments copied forward as before.
- **Waiting operations** are `api::auto_approval::waiting`: one `policy.accept`
  item per broad policy that is on, naming the operations `choose` would give it
  once accepted, for the roles holding `approvals:delegate`. Reconciled for the
  workspace whenever a version is published (every workspace with a broad
  policy on, for a platform skill), a binding changes or a policy is written;
  replaced when the operations change, resolved when they are accepted and
  cancelled when the policy stops standing. Migration 0059 keeps it to one open
  item per policy. The inbox links to the policy's page.
- **The page** is Settings → Auto-approval, `/settings/auto-approval`; a new
  policy at `/settings/auto-approval/new` is prefilled from `requires`,
  `selector`, `max_risk`, `skill_id`, `matches`, `agent_id` and one `field` per
  bound field. The approval prompt links there once the workspace has approved
  the same `requires` three times, for somebody who could set one.

### As built: risk from the amount

- **Declarations.** `amount`, `bands` and `floor` are read in
  `api::skill::frontmatter` as one-line mappings and checked by
  `egress::amount::Declaration::check`: the amount bound and in `numbers`, a
  currency of three upper-case letters, `minor_units` at most 4, bands above
  zero, only for levels below `risk`, rising, and none above the floor of a
  higher level; floors rising. Stored as JSON in `skill_version_gates.amount`
  (migration 0066) and copied with the gates.
- **Workspace bands.** `approval_bands` (migration 0067): a row per currency,
  or per operation named as a policy names one (`skill_id`, `requires`,
  `matches`). The operation's row wins over its currency's, either over the
  operator's defaults; a row replaces the bands whole, and deleting it goes
  back to what stood beneath. Checked when written against the floor -- of the
  operation, or of every money operation an agent reaches in that currency --
  and the floor wins again when a request is assessed, so a currency row
  written before a skill's floor rose is *held at the floor*, and the page says
  so. Read and written as policies are (`approvals:delegate` with
  `approvals:answer`, from a browser session). Applied by
  `api::auto_approval::declared_in`, which every resolution now goes through.
- **In the token.** A `PolicyGrant` for a money operation carries the bands it
  runs under (`banding`), `max_risk` -- the tighter of the policy's bound and
  the operator's ceiling -- and `currency_per_day`. All three are left out when
  absent, so a grant for an operation with no amount hashes as it did.
- **At the gateway.** `PolicyGrant::admits` works out the request's risk with
  `egress::amount::risk` exactly as *What a request's risk is* says, and admits
  nothing above `max_risk`. A policy with a daily total in the currency admits
  no request whose amount it cannot read, since it could not be counted. The
  record writes `currency` and `currency_amount`, and its insert also holds the
  day's total per currency to the tighter of the committed and the row's.
- **Resolution.** A selector, and the operator's ceiling, compare against the
  *lowest* risk a gate's requests can work out to (`Declared::reach`); the
  gateway then holds each request to the bound by its own amount. So the
  ceiling now caps computed risk, not only base risk: bands decide where a
  request falls beneath it.
- **Daily totals.** `approval_policies.daily_totals`, `{"GBP": 15000}` in minor
  units, on any selector. A policy with totals covers money operations only in
  a currency it totals; one moving no money is covered as before.
- **One unit per currency.** What is kept for a whole currency -- its row of
  bands, a policy's daily totals, a day's `currency_amount`, the offer's
  figures -- is in the currency's standard minor units by ISO 4217
  (`egress::amount::standard_minor_units`: a short table, two for anything
  not in it), and an operation's own row is in its amount field's units
  (migration 0069). A currency's band reaches an operation counting in other
  units rounded down (`from_standard`), and a request's amount reaches the
  day's total rounded up (`to_standard`), so a conversion that loses
  precision always lands on more risk and a larger day.
- **Routing.** `api::gated` works out the risk of the refused request from the
  workspace's bands (the API's reading rather than the token's: it lets nothing
  out, only chooses who is asked) and, for high, asks the roles in
  `approval_routes` (migration 0068) that still hold `approvals:answer`; none
  named, or none left, and every approver is asked as before. The item's
  payload gains `risk`, and `money` (`amount`, `currency`, `minor_units`,
  `display`, `field`) where the amount could be trusted.
- **The API.** `GET/PUT /v1/approval-bands` (`DELETE .../{id}`): every money
  operation an agent reaches, with its defaults, floor, effective bands, their
  source and whether the floor holds, each as money (`display`: `£50.00`), the
  currencies with their rows, and rows naming operations nobody reaches.
  `GET /v1/approval-routes`, `PUT /v1/approval-routes/high` (`role_ids`).
  `GET /v1/approvals/amounts?requires=&days=` -- for `approvals:answer`, this
  workspace's resolved questions of that act carrying an amount, per currency:
  count, largest, total, and `suggested_low`, the first round amount above the
  largest. Policies take `max_risk` on an act or operation selector and
  `daily_totals`, so the one line is a band (`PUT /v1/approval-bands`) and a
  policy (`selector: act`, `max_risk: low`, `daily_totals`, the agent, an
  expiry).
- **The pages.** A queue item's `risk` and `money` are read by `lib/money`
  (`riskOf`, `moneyOf`); the approval prompt and the queue rows show a
  `RiskBadge` and the amount's `display`, the prompt beside the bound number
  it came from, which stays. Amounts are typed in whole units and sent in
  minor ones (`toMinor`, on the digits), and a figure worked out on the page
  is written by `formatMoney` as the API writes `display`. Settings →
  Auto-approval has, in order: the one line (`OneLinePolicy`), the policies,
  the bands per currency and per operation (`ApprovalBands`), and who is
  asked about high risk (`HighRiskRoute`). The editor takes `max_risk` on any
  selector and daily totals per currency, and says a test on a declared
  amount field as money.
- **The one line** is one write, `POST /v1/approval-policies/one-line`
  (`requires`, `currency`, `low` and `daily_total` in the currency's standard
  units, `agent_id`, `mode`, `expires_at`): the low band of each operation of
  the kind in that currency whose low is not that already -- per operation,
  never the currency's row, which would move every other operation in it --
  in the operation's own units, rounded down, keeping a medium bound still
  above it; and the policy. Everything is checked first and written in one
  transaction, so a band the floor refuses leaves no policy and a policy
  refused leaves no band. It is prefilled from
  `/settings/auto-approval?line=&currency=&low=`. Its counts are one a turn
  and twenty a day, which the full editor changes.
- **The offer** reads `/v1/approvals/amounts` first, in the request's
  currency or else the one most approved: "12 charges approved in the last
  30 days, all under £40.00." -- the figure being the largest plus one,
  rounded up at its leading digit -- linking to the one line at
  `suggested_low`. An act with no amounts falls back to the count and the
  full editor, as before. Neither is asked for on a follow-up.

#### Deviations

- `max_risk` is now allowed on an act or operation selector (migration 0067
  relaxes the check), which the design did not say; it is what "charges under
  £50" needs without sweeping in every other low operation.
- Low that no policy covers asks every approver, as medium does: there is
  nobody else for it to go to.
- The offer counts only questions raised since they carried `money`.

### What is not built

- **An edited policy is not re-committed mid-turn.** A trade-in drops it, and
  the turn asks a person for what it covers until the next turn commits it on
  its new terms. Adding to a running turn's token is what a trade-in never does.
- **A gate's publication is its version's.** A body-only edit copies the gates
  onto a new version, so they read as published again: the queue says they
  wait, and a broad policy covers them in trial until accepted, though nothing
  about them changed.
- **The admin agent proposing a policy**, which waits for the admin agent.
- **Bands mid-turn.** A band changed while a turn runs reaches it from the
  next turn, as a policy edit does.
- **A policy suspending itself**, and **operator gates that follow the
  credential**, still unsettled above.
