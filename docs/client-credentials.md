# Client credentials

How an egress rule reaches an API that wants an OAuth 2 access token rather
than a static key, without the platform starting to store secrets.

Built: the rule, its commitment, the exchange and the cache. It rests on [egress.md](egress.md), which is built, and
sits in tier 1 of [integrations.md](integrations.md): the operator's own APIs,
configured by the operator.

## What is wanted, and what is not

Today a rule names a host, a header, and an environment variable holding that
header's value; the gateway reads the variable and attaches it. That covers an
API key. It does not cover an API that issues keys of its own: a client id and
secret are exchanged at a token URL for an access token that lasts an hour, and
only the access token is accepted on requests.

This is the two-legged grant, RFC 6749 section 4.4 -- the platform
authenticating as itself, with credentials an operator was issued. It is *not*
the three-legged flow tier 2 needs, where each workspace authorises outturn
against its own account and a refresh token is held per workspace.
integrations.md is right that client credentials are the wrong grant for that
case, and nothing here changes it: a client-credentials rule is a static
credential with an extra step, which is exactly why it can keep the property
static credentials have.

That property is the one in AGENTS.md: **credentials are named, never stored.**
Both the client id and the secret are environment variables on the gateway,
named by the rule. The access token is derived from them, lives in the
gateway's memory, and is never written anywhere -- see below for why that
includes the database.

## The rule

A rule carries one of two credential shapes, never both:

| | Static | Client credentials |
|---|---|---|
| What is attached | `header: <value of credential_env>` | `Authorization: Bearer <access token>` |
| Named variables | `credential_env` | `client_id_env`, `client_secret_env` |
| Also | `header` | `token_url`, `scope`, `client_auth` |

- **`token_url`** is a full https URL, path included. The path is not a detail:
  multi-tenant identity providers put the tenant in it
  (`login.example.com/<tenant>/oauth2/token`), so a rule committed to the host
  alone could have its secret presented to somebody else's tenant.
- **`scope`** is a single string, space-separated, sent exactly as written and
  nullable for providers that want none. Not normalised -- sorting a list into
  canonical order would be a second reading of the field for the commitment to
  disagree with, and a provider that cared about order would be sent something
  nobody wrote.
- **`client_auth`** is `basic` (the default, and the one RFC 6749 obliges a
  server to support) or `post`, for the providers that only read the id and
  secret from the form body. It is part of the rule because it decides where
  the secret travels.
- **`header` is not configurable.** The grant defines `Bearer` in
  `Authorization`; a provider wanting something else is not speaking this
  grant, and a free-form header beside a token would be one more field for the
  commitment to cover in exchange for nothing.

`audience` and `resource` parameters are left out. Some providers want one;
none of the APIs in front of us does, and a generic "extra form fields" column
is a place to smuggle anything. Add the specific parameter when a specific
provider demands it.

In the schema these are nullable columns on `egress_rules`
(`migrations/0021_client_credentials.sql`) with check constraints saying
exactly one shape, or neither, is present. In `runtime::egress::EgressRule`
the exchange is one optional `client` field holding all of its parts, so half
an exchange is unrepresentable; a rule carrying both shapes is refused by the
API when written and by the gateway if a row edited by hand ever says so. An
optional field rather than an enum over the two shapes, so a static rule
encodes exactly as it did -- the field is left out when absent -- and every
place that builds one did not have to change. `rules::check_url` is unchanged: the rule still matches on its
host, and the shape only decides what is attached once it has.

## The commitment covers every field

`commit::leaf` hashes every field of a rule today because "a rule whose
credential could be swapped for another host's is a rule that leaks it". A
client-credentials rule has more ways to be swapped, and each needs covering:

- `token_url`, or a runtime keeps the committed host and variables and points
  the exchange -- which carries the secret -- somewhere it chose.
- both variable names, or it borrows another rule's secret for this rule's
  token URL.
- `scope`, or it asks for more than the rule was written for. The provider
  enforces what the client may have, but the rule is the operator's statement
  of what this use of it needs.
- `client_auth`, since it moves the secret between a header and a body.

A client-credentials leaf gets **a tag of its own** -- `TAG_LEAF_CLIENT` beside
`TAG_LEAF` -- and hashes all of the above through `hash_fields` after the
workspace id and host. Two reasons for a new tag rather than appending fields
to the existing leaf. A static rule's leaf stays byte-identical, so commitments
over rules nobody changed do not move across the deploy. And no static rule can
ever hash like a client-credentials one, which length-prefixing alone promises
only while the field lists happen to differ in length.

The proof format does not change: it carries the rule, and the rule now carries
more. The runtime passes the rule through rather than reading it, but it does
deserialise it, and a runtime built before this would drop the new fields and
send a rule that fails to verify. So the three tiers deploy together for this
change, or the runtime gains the type first. Failing closed is the right
direction for that mistake to go.

The existing test that a rule cannot be swapped for another host's credential
grows a case per field above.

## The token endpoint is a host like any other

The exchange is an outbound request carrying a secret, made by the gateway, to
a URL a row named. That is precisely what `resolve_and_vet` exists for, so the
exchange goes through the same transport as an agent's request:

- resolved once and the connection pinned to the answer;
- refused if the address is not plainly public, unless the operator's
  internal-hosts list names it, as for any other host;
- https, unless the operator allowlisted the host -- the same exception
  [egress.md](egress.md) makes for a static credential, for the same reason;
- redirects not followed. A token endpoint answering 302 has named a host
  nobody vetted, and following it would hand the secret there.

**The token endpoint is not an egress host.** It does not need a rule of its
own and is not added to the workspace's list: the agent never reaches it, and
allowing `login.example.com` so that the exchange could happen would also let
the agent `fetch_url` it. It is reachable *as the token URL of a committed
rule*, and that is all.

The response is read with a small limit -- 64 KiB is generous for a JSON object
with four fields -- and a short timeout, ten seconds, because a turn is waiting
on it. `token_type` must be `Bearer`, compared case-insensitively as the RFC
says; anything else is refused rather than attached under the wrong scheme. The
token must be a valid header value and is marked sensitive like the static
credential is.

## Where the token lives

**In memory, per gateway replica.** Not in the database the circuit breaker
uses.

The breaker is in Postgres because its job is collective: one replica
discovering an outage alone still lets every other replica storm the provider.
A token cache has no such job. The cost of each replica holding its own is one
exchange per replica per token lifetime -- ten replicas and hour-long tokens is
ten requests an hour to an endpoint built to serve them. Against that, a token
in a table is a working bearer credential in a row, reachable by every backup,
read replica and support query the table is, for the hour it lasts. The
property AGENTS.md states is that nothing which reads a table can leak a secret
by reading it, and the token is a secret for its lifetime. Sharing it saves
requests nobody was short of.

The cache is keyed by the rule's leaf hash. That is per workspace, because the
workspace is in the leaf, and per everything else the leaf covers -- two rules
with different scopes are two tokens, as they should be. A rule that changes is
a different key, and the old entry ages out.

Each entry holds the token and when it expires, computed from `expires_in` at
the moment the response arrived rather than when the request left. A response
with no `expires_in` is cached for five minutes: the RFC lets a server leave it
out, and treating that as "forever" is how a deployment ends up attaching a
token the provider revoked yesterday.

**Refreshed on demand, before it lapses.** When a request needs the token and
less than a minute, or a tenth of its lifetime, is left -- whichever is longer
-- it is exchanged again. No timer, for the reason integrations.md gives for
refresh tokens: a token nobody is using needs no keeping alive. Single-flight
per key, so a burst of concurrent calls after expiry makes one exchange and the
rest wait on it. There is no refresh token to manage; RFC 6749 says a client
credentials response should not carry one, and if one arrives it is ignored.

**A 401 from the API evicts, and does not retry.** A provider can revoke a
token early, and the cache cannot know. On a 401 from a host whose rule
attached an exchanged token, the entry is dropped and the 401 goes back to the
agent as it is today; the next call exchanges afresh. Retrying inside the
gateway would be cleaner to watch and is the wrong default --
[idempotency.md](idempotency.md) is about exactly the request that may or may
not have landed, and the gateway should not be the place that decides a 401 is
the safe kind.

A restart loses the cache, and costs one exchange per rule per replica. That
is the whole price of not storing it.

## What a failed exchange tells the agent

The agent cannot fix any of these. It did not write the rule and cannot see the
secret, so what it needs is to know whether trying again could help, in the
plain text every other refusal in `gateway::egress` uses. The operator needs
the detail, which goes to the gateway's log with the workspace and host.

| What happened | Status to the runtime | What the agent reads |
|---|---|---|
| A variable is unset | 403 | as today: this host's credential (`CLIENT_ID_ENV`) is not configured |
| Token URL refused by the vetting above | 403 | this host's credential could not be obtained: the token endpoint is not reachable from here |
| 400/401 with an RFC 6749 `error` | 403 | ...could not be obtained (`invalid_client`). This is a configuration problem; retrying will not help |
| 5xx, timeout, unreachable | 502 | ...could not be obtained just now; the provider's token endpoint did not answer |
| 2xx that is not a usable token | 502 | ...the token endpoint's answer was not one this can use |

Only the `error` code is passed on, never `error_description` or the body. The
code is a fixed vocabulary that says which side is wrong; the description is
free text from a server that has just been sent a secret, and some providers
echo the client id or more back in it. A body that might contain a credential
does not go to a model.

A configuration failure is remembered for thirty seconds per key, so an agent
that ignores the advice and calls ten times makes one exchange rather than ten.
Not longer: an operator who fixes the variable should not wait to find out it
worked. A 5xx is not remembered -- the provider's own backoff is the provider's
business, and the breaker is for model calls.

## What this does not settle

The binding problem in integrations.md -- "a credential is bound to its host"
-- gets wider, not narrower. A rule now names two destinations for a secret:
the token URL receives the client secret, and the API host receives the token.
Someone who can write rules can name a real secret's variables beside a token
URL they control, and the secret is gone on the first exchange. Today that is
the same exposure as naming `STRIPE_KEY` beside a host of one's choosing, and
it is held by the same thing: rules carrying credentials are written by an
operator. Until the binding exists, the API should refuse a client-credentials
rule from anyone a static credential would be refused from, and when it does
exist it binds the variables to both destinations at once.

## The wizard

[openapi-wizard.md](openapi-wizard.md) already reads `securitySchemes` to
prefill a static rule's header. A scheme of type `oauth2` with a
`clientCredentials` flow carries `tokenUrl` and a map of `scopes`, which is
most of a rule of this shape: the wizard can propose `token_url` and offer the
scopes to choose from, leaving the operator to name the two variables. That
waits on this being built; nothing in the wizard changes until then. A relative
`tokenUrl` is resolved against the specification's own URL, as OpenAPI 3.1
says, and the result is still only a proposal an operator saves.
