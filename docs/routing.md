# Routing

Which model answers, whose key pays, and what happens when it fails. Part
built, part designed; each section says which.

## Who this serves

An operator runs outturn. Tenants are the operator's customers, and tenants
have customers of their own. The operator's business decides who owns the
model relationship, and three arrangements have to work on one gateway:

1. **Operator-provided.** The operator holds every key, routes as it likes for
   cost and reliability, and bills tenants for usage or folds it into the
   price. An HOA management platform whose workflows are certified against one
   model works this way.
2. **Tenant brings a key.** The tenant pays their provider directly and pays
   the operator only for hosting. A customer-service tenant with its own
   Anthropic account.
3. **Operator runs its own inference.** A vLLM serving an open model, sold at
   a margin, with a hosted model behind it for when the vLLM is full.

Fallback has to work across all three, and the bill has to say afterwards
which one paid for which call.

## Traffic classes, not model names

Built. Nothing above the gateway names a model. An agent's policy names a
*traffic type* -- `assistant` today -- and the gateway resolves it through
`traffic_routes` to an ordered list of attempts: protocol, base URL, model,
credential. A skill will name a class the same way. The tenant and the
tenant's customers never learn which model served them unless the tenant is
the one holding the key.

`assistant` is the generic class: realtime chat and scheduled work of the same
character. More classes come as they are needed -- `customer-service`,
`summarise`, `compaction` -- and each is a row set, not code.

A class may carry a **default priority** for jobs that arrive without one. It
is a fallback, not a derivation: priority is a property of the trigger (is
somebody waiting?), class is a property of the task (what is good enough?),
and the same class runs at both. A message handler enqueues realtime, a
scheduler or webhook enqueues background, and only a job that says nothing
takes the class's default. `summarise` and `compaction` default to background
because nobody is ever watching them.

## Rows as the whole of routing

Built: `traffic_routes` with a null tenant meaning the operator's default and a
tenant's rows replacing it; attempts tried in priority order, skipping any
whose circuit is open.

Designed: two more columns on each row.

**Credential owner.** Today every credential is an environment variable on the
gateway pod, so every key is the operator's. A row will say whether its key is
*operator-held* (an env var, as now) or *tenant-held* (a key the tenant
entered, stored by the gateway encrypted under a master key from its own
environment). The gateway is already the only tier that holds credentials, so
this widens nothing; the API proxies the tenant's submission and learns only
that a key is on file and its last few characters.

A tenant's list can then mix freely: Bedrock with their AWS credentials, then
Anthropic direct with their own key, then -- if offered -- the operator's
model. Same-model fallback (Bedrock to Anthropic direct) needs nothing more
than two rows.

**Fallback kind.** Whether stepping to this row keeps the model or changes
it. Same weights through a different door is safe to fall through silently.
A different family is allowed but visible: recorded on the reply and the
ledger as a model change, shown in the UI, and something a tenant opts into
per route rather than gets for free. Cross-provider failover is not honest
until the durable transcript is separated from the per-provider projection
(see AGENTS.md), because a transcript that has been through two families
holds artifacts each rejects. A skill certified on one model declares that it
accepts same-model fallback only; the tenant cannot loosen that, the operator
can.

The editor infers the kind from model names so the obvious cases need no
labelling.

## The surcharge fallback

Designed. A tenant with their own key may opt into "during provider outages,
fall back to the operator's model; additional charges apply." It is just the
operator's rows appended after the tenant's, and one fact on the bill: those
calls were paid with the operator's credential.

- Off by default, per tenant.
- The operator decides whether to offer it at all. A hosting-only tenant has
  no usage relationship to charge against and never sees the checkbox.
- The breaker decides when it engages, not the tenant. The tenant's rows are
  always tried first, so nobody routes to the operator's model by preference.
- Fallback is per model call. A turn that fails mid-stream on the tenant's
  key is retried, and the retry may land on the operator's model. The
  transcript, not the session, is the unit of billing, and the ledger says
  which call went where.

## Two breakers, not one

Built: a breaker shared across gateway pods through Postgres, keyed by
endpoint (protocol and base URL), ignoring client errors so a rejected
request or a rate limit does not mark a provider dead.

Designed: split it, because two different things trip it once tenants hold
keys.

- **Endpoint health** stays cross-tenant. Connection refused, timeouts, 5xx.
  A dead provider is dead for everyone and one pod learning it should spare
  the rest. Keyed by endpoint, as now. An overloaded vLLM is endpoint-wide and
  belongs here.
- **Credential health** is per credential. 401 and 403 mean this key is bad;
  429 means this key is throttled. Neither says anything about the endpoint or
  about anyone else's key. Keyed by credential, a short cooldown for 429 and a
  longer one for 401, never propagated to the endpoint breaker. A tenant's
  expired key must not take a provider down for the other tenants.

The walk skips a row if either breaker is open, and the ledger row that
eventually serves records which one it was. "Why did we fall back at 3pm" is
answered with "your key was rate-limited", not "Anthropic was down", which
matters when the surcharge is on the bill.

Today a 429 does not count at all, so a tenant hammering their own key fails
the row and moves on every call. With a per-credential breaker the row is
skipped for the cooldown instead, which is cheaper and is the backoff the
provider asked for.

## Attribution

Every call is a row in the usage ledger (see [usage.md](usage.md)) naming the
endpoint, the model actually served, the credential owner, and the fallback
kind if it was one. The bill hangs off that, not off routing configuration,
because configuration changes and the ledger does not.

## Order of work

The ledger first: routing changes are what produce multi-provider turns, and
the current per-message usage summary is wrong for those. Then credential
owner and fallback kind on the rows, tenant-held keys in the gateway, the
credential breaker beside the endpoint one, and the surcharge checkbox. Then
more traffic classes as skills need them.
