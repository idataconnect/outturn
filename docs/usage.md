# Usage

What was spent, by whom, for which customer, on which model. Built.

## The ledger

`usage_ledger` holds one row per model call. Not per turn: a turn that fell
back to a second provider mid-way has two rows naming two endpoints, and a
turn that failed after three calls has three rows, because those calls cost
money whether or not the turn finished.

Every dimension a bill might be cut along is on the row:

| Column | Says |
|---|---|
| `workspace_id` | Whose ledger. The partition key. |
| `agent_id`, `session_id`, `user_id` | Which agent, conversation and person |
| `account` | The workspace's own label for whose conversation this was |
| `reply_id`, `job_id`, `round` | Which reply, which job, which call within the turn |
| `traffic_type` | What class of work it was |
| `endpoint`, `model` | Who answered, and with what. The model that *actually* served, which routing may have chosen |
| `credential_owner` | Whose key paid: `operator` or `workspace` |
| `fallback` | `none`, `same_model` or `cross_model` |
| five token columns | As the provider reported them, normalised |
| `provider_usage` | The provider's usage object verbatim, for dimensions the columns do not model |
| `service_tier` | The price tier that served it, where a provider has them |

Tokens, never prices. Rate cards change and disputes happen, and a ledger that
stored a computed cost would have to be corrected where one that stores tokens
is re-priced by whoever is billing.

The five normalised columns are what every provider agrees on and every rate
card needs. They are not a superset and never will be: cache writes priced by
TTL, service tiers, long-context thresholds, server-side tools billed per call,
audio and image tokens. So the provider's usage object is kept whole beside
them. The columns build today's bill; the raw object lets yesterday's calls be
re-priced under a dimension nobody thought to normalise, without a backfill.
For the Anthropic protocol the gateway's translation is lossy by design, so
the original rides through under an `anthropic` key.

Append-only. Nothing updates or deletes a row, so an export of a closed window
returns the same rows every time it is run.

## The account label

A session may carry an `account`: free text the workspace sets when opening the
conversation, meaning whatever the workspace's business means by it -- an HOA, a
customer number, a matter. The platform never interprets it. The ledger copies
it onto every row the session produces, so a workspace can join its bill to its
own records without the platform knowing what those records are.

## The platform workspace

Work the platform does on its own initiative -- titles, summaries, anything
that comes later -- bills to a reserved workspace, `platform`, with a fixed id
and a trigger that refuses its deletion. A reserved row rather than a null
keeps partitioning uniform and foreign keys real, and makes "what did the
platform itself spend" the same export as everyone else's.

## The export

`GET /v1/usage`, needing `usage:read`. Parameters:

- `from`, `to`: RFC 3339, inclusive start, exclusive end. A closed month is
  `from` the first and `to` the first of the next.
- `after`: the `next` of the previous page. Ids are UUIDv7, so id order is
  time order and the id doubles as the cursor.
- `limit`: up to 5000, default 500.
- `workspace_id`: system administrators only. Everyone else gets their own.

The response is `{ entries: [...], next: <id> | null }`. A null `next` means
the window is exhausted.

The export is the product. An operator runs it across workspaces and bills them.
A workspace runs it on its own ledger, joins `account` to its records, applies its
own rates, and bills its customers. Nobody needs a billing system inside
outturn, which is good, because every customer's is different.

## How rows get written

The runtime host emits a `usage` event on the turn's stream the moment each
model call returns, carrying the endpoint and model the gateway named, whose
credential paid (a header the gateway sets), and the provider's token counts.
The API writes a ledger row as each arrives, before the turn ends. A failed
ledger write is logged at error, because a wrong bill is an operator's problem
and a quiet one is worse than a loud one.

## The summary

`GET /v1/usage/summary`, needing `usage:read`. What the dashboard reads.

The export stays the product -- this is a reading of it, computed where the rows
are. A browser folding the ledger itself would page every row of a window down
the wire to show one number, which is tolerable on a dev machine and wrong on a
real deployment.

Parameters are `from` and `to` as the export has them, defaulting to the last 30
days ending at the *next* midnight, so today is a whole bucket rather than a
partial one that reads as a collapse in traffic. A window is always closed, and
bounded at 370 days: the statement behind it scans every partition, and an
unbounded one is a way to ask the database for everything by accident.

Scope works the way the export's does, with one addition. `workspace_id` names a
workspace, and only a system administrator may name one that is not their own.
`scope=all` sums every workspace, and only a system administrator may ask -- an
absent scope means the caller's own workspace, so a page cannot widen itself by
forgetting a parameter.

The response carries the window's totals, a bucket per day, and the window cut
by workspace, model, agent, account and `usage_source`. Days with no rows are
filled in with zeros rather than omitted: a chart drawn from a series that skips
its empty days draws a quiet day as no day at all, which reads as a shorter
window rather than an idle one. Each dimension is ordered by tokens and capped,
with the tail folded into a single "others" entry rather than dropped, so
nothing is lost between a dimension's slices.

They do not all sum to the window's total, though, and the page does not
pretend they do. A dimension whose column is nullable -- `account` most
obviously, `agent_id` for the platform's own work -- has rows in no named
slice, which arrive as a slice with a null key rather than as an invented
label. Each panel's shares are therefore against its own dimension, not
against the headline figure.

Tokens throughout, never prices, for the reason the ledger gives. `by_source` is
what keeps that honest on a page: a total that mixes reported and unmeasured
rows without saying so presents an estimate as a fact, so the dashboard states
what share of a window nobody measured rather than folding it in silently.

## The dashboard

The overview page reads the summary and nothing else, so everything on it is
about model calls -- it says nothing about turns that never reached a model,
which is worth remembering before it grows panels that look like they cover
everything.

The platform's own work -- naming a session, compacting a transcript -- carries
no agent, because no agent asked for it. It appears as an unattributed slice,
and the `traffic_type` cut beside it is what says what that work was; the agent
count includes it as one more answer to "who answered", so the tile and the
panel below it cannot disagree.

Two things on it are design rather than decoration. Cache reads are drawn beside
the daily stack rather than in it: on a transcript-heavy workload they run an
order of magnitude above the rest, and stacking them leaves completion tokens a
few pixels tall. The separation is real rather than cosmetic -- a cache read is
context being re-read, priced differently by every provider, not new work the
way a prompt or a completion is -- and the alternatives are worse, a log axis
making a tenfold difference look small and a second y-axis never being the
answer. The line is dashed and its own maximum is written into the legend,
because a reader must be able to see that it does not share the axis beside it.

The other is that every chart has a table behind it carrying the same figures,
and that the series colours are checked rather than chosen: they come from the
theme's own ramps, stepped until both modes cleared a validator for the
lightness band, the chroma floor, adjacent-pair separation under protanopia and
deuteranopia, and contrast against the surface. `ui/src/lib/viz.ts` holds them,
and the fixed slot order is the safety mechanism -- anything past the last slot
takes a neutral rather than a hue nobody checked.

## Not yet

- `credential_owner` is always `operator` and `fallback` always `none`, because
  routing does not yet support workspace-held keys or record fallback kind. The
  columns exist so the bill does not have to be re-derived when it does; see
  [routing.md](routing.md).
- Nothing spends on the platform workspace's behalf yet. The row exists for when
  something does.
- Retention. The ledger grows with every call and is never pruned. A billing
  obligation decides how long rows must be kept, and that is a policy the
  operator sets, not a default the platform should guess.
