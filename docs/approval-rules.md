# Approval rules

Built; *As built*, at the end, says where and how, and where it differs from
what follows. How auto-approval is *authored and shown*; what is enforced,
and where, stays as [auto-approval.md](auto-approval.md) describes it.

## What is wrong now

One question -- *may this endpoint go through without asking, and up to how
much?* -- is answered in three places, each in its own vocabulary:

- **the skill** declares a risk, an amount field, a currency, default bands
  and a floor;
- **Settings → Auto-approval** holds *bands* (per currency, or per operation)
  that move a request's risk, and *policies* that let risks through, per kind
  of request, per agent, with caps;
- **the approval prompt** offers a shortcut into a one-line form.

To know what happens when Front Desk charges £60, a person combines the
floor, a currency band, an operation band, a policy's maximum risk and its
daily total. The page puts that model on screen as it is stored. Its common
case is keyed on a skill's approval word (`requires: charge`), which spans
every skill and every endpoint using that word, so the page cannot say which
endpoint it is about. And it is a sentence with inputs in it, which reads
well for one skill in one currency and nowhere else.

## The idea

**A rule belongs to an operation.** Every gated endpoint an agent can reach
has exactly one rule in force, said in plain words:

- *Always asks*
- *Automatic under £50, up to £500 a day* -- for some agents, until a date,
  maybe in trial
- *Automatic* -- for an operation with no amount
- *Never automatic* -- the skill says `auto: never`, with its reason

One table lists them, one panel edits one, and the same panel opens from
wherever the question comes up: the table, the skill's own page beside the
operation, and an approval in the queue. There is nothing else to learn.

## What goes

- **Workspace bands.** The amount a rule lets through is part of the rule.
  A band was a way to say "£50 is small" once for many operations; the table's
  bulk edit says it for many operations without a second concept.
  `approval_bands` is migrated into the rules it was feeding and dropped.
- **Act selectors.** A rule over every operation whose approval word is
  `charge`, across skills, is the thing nobody can read. Existing ones are
  expanded into one rule per operation they cover today, keeping their caps,
  agent and expiry, and the selector is removed.
- **The one-line form**, the *By currency* section and the explanatory
  prose around them.

## What stays

- Enforcement at the gateway, from the turn token; the condition language and
  its fail-closed meanings; required caps; expiry; lapsing when the person who
  set it leaves; trial; `approvals:delegate` beside `approvals:answer`.
- The skill's declarations, which become what a rule may not exceed:
  `auto: never` locks the rule, and the floor is the most the amount limit may
  be set to. The operator's bands keep deciding a request's risk, and so who
  is asked, when no rule lets it through.
- **Broad rules** -- "every low-risk operation without an amount" -- kept for
  the case they suit, a skill with forty read-only endpoints. They live in
  their own short section under the table, and each row they reach says so:
  *Automatic, by the broad rule for low-risk operations*. An operation's own
  rule overrides a broad one.

## The table

One row per gated operation any agent can reach, grouped by skill.

| Column | Shows |
|---|---|
| Operation | The skill's summary, and `POST /charges` under it |
| Agents | The agents that can reach it |
| Rule | A chip: *Always asks*, *Automatic under £50 · £500/day*, *Trial*, *Never* |
| Last 30 days | How often it asked, and how many of those were approved |

Filters for skill, agent and rule; a search over operations. Selecting rows
offers *Set a rule for these*, which is the bulk edit, available when the
selected operations share a currency or have no amount.

Rows are operations, not policies. An operation with different rules for
different agents shows one line per rule inside its row.

## The panel

Opens beside the table, or from a skill or an approval, on one operation:

1. **What it is.** Summary, method and path, skill, the agents that reach it,
   and what the skill fixes (`never`, a floor).
2. **The rule.** A choice of *Always asks* or *Automatic*. Automatic shows
   labelled fields, each on its own line:
   - *Only under* -- an amount, with the currency beside the input and the
     floor as its hint; absent for an operation with no amount
   - *At most a day* -- a total in the same currency, and a count
   - *Agents* -- all, or chosen ones
   - *Until* -- a date, ninety days out by default, at most a year
   - *Start in trial* -- on by default for a new rule
3. **What it would have done.** Against the last thirty days of this
   operation: how many requests it would have let through, their total, and
   first, any a person declined. Updated as the fields change.
4. **History.** Who set the rule, each change, and what it approved.

Saving writes one rule. Nothing about bands, risks or selectors is on it.

## Where it opens from

- **Settings → Approvals** (renamed from Auto-approval, since it now shows
  every gated operation, including the ones that always ask).
- **The skill's page**, which gains an *Approvals* column on its operations:
  the same chip, opening the same panel. Declaring the gate stays in the
  skill; the rule a workspace sets on it is one click away from it.
- **An approval in the queue**: *Stop asking for charges like this* opens the
  panel on that operation, with the amount prefilled above the one asked
  about and the evidence already showing.

## The API

`GET /v1/approval-rules` returns every gated operation reachable in the
workspace, each with its rules in force, what the skill fixes, and the
counts the table shows. `PUT /v1/approval-rules/{skill}/{operation}` writes
one operation's rule, and `POST /v1/approval-rules/bulk` the same rule for
several. `GET .../{skill}/{operation}/preview` answers the panel's *what it
would have done*. `/v1/approval-policies` and `/v1/approval-bands` are
removed; broad rules move under `/v1/approval-rules/broad`.

Stored as policies with an operation selector, an amount condition and a
daily total, as the gateway already reads them. The gateway does not change.

## Money

Every amount is entered and shown in the reader's locale, with the
currency's own decimals, and sent in minor units. A currency is never shown
without the operation it belongs to.

## Migration

One migration, in order: workspace bands become each affected operation's
amount limit where a policy let that risk through; act policies are expanded
to operation policies; both old tables' rows are removed. A workspace whose
rules change meaning in the move -- a band that let through more than its
policy's daily total, say -- gets an inbox item naming the operation and what
changed. Nothing is widened by the migration; where a choice is ambiguous,
the narrower reading wins.

## Not settled

- **Per-agent rows.** Showing one line per rule inside an operation's row is
  the starting point; if many workspaces set per-agent rules, agents may want
  to be a grouping of their own.
- **Who is asked.** Routing high risk to named roles stays as it is for now.
  It could become part of the rule -- *above £200, ask a manager* -- which
  would be the last thing still keyed on risk.

## As built

- **Reading.** `GET /v1/approval-rules` (`api::auto_approval::rules::list`):
  every gated operation an agent in the workspace reaches, once, with its
  skill, a name taken from the file declaring it, the agents, its amount field
  and currency, what is fixed (`never`, `beyond_deployment`, and `most`: where
  the deployment's cap or the skill's high floor stops an amount), the rules in
  force -- `choose`'s answer for each agent, grouped by policy -- and its
  questions in the last thirty days. A rule's `under` is the tighter of its
  amount test and where its risk bound ends, so an older policy "up to low"
  reads as an amount too.
- **Writing.** `PUT /v1/approval-rules` takes one rule for one or several
  operations, for one agent or all: automatic with `under` and `daily_total`
  in the currency's standard units, counts, mode and expiry; or not, which
  revokes the operations' own rules and, where a broader one still speaks for
  them, adds an exclusion. Stored as an operation policy with an amount test
  and a daily total, which the gateway already enforced, so it did not change.
  Everything is checked before a transaction writes any of it. A rule for one
  agent lifts an exclusion for every agent by replacing it with exclusions for
  the others.
- **Preview.** `GET /v1/approval-rules/preview?skill_id&requires&matches&under&daily_total`
  replays the rule against the last thirty days of its operation's questions
  with `api::replay`.
- **The page** is Settings → Approvals (`pages/Approvals.tsx`), the panel
  `components/ApprovalRulePanel.tsx`, beside the table from 1280px and over it
  below. A skill's page lists its gated operations with the same chips and
  panel (`SkillApprovals`). The approval prompt's offer links to
  `/settings/approvals?requires=&under=`, which opens the operation's panel
  where one operation needs that act and narrows the table where several do.
- **Conversion.** `api::auto_approval::convert::run`, at API startup under an
  advisory lock, in one transaction: each act policy standing becomes a rule
  on each operation it reached, held to the amount it let through under the
  bands as they were; a risk or operation policy whose workspace band was
  narrower than the operator's gets an operation rule holding it there; the
  act policies are revoked and the bands deleted. Run again it finds nothing.

### Deviations

- `/v1/approval-policies` stays, for broad rules and for showing any policy a
  link names, rather than moving under `/v1/approval-rules/broad`.
- Several operations are one `PUT` with a list, not a separate bulk endpoint.
- No inbox item for a changed workspace: the conversion keeps every amount
  where it was, and the only narrowing is that a rule covers no operation
  published later, which an act policy did only in trial.
- `approval_bands` is left empty rather than dropped, so the conversion can
  read it on a deployment that upgrades past this release; a later migration
  drops it.
