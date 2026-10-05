# Integrations

How a workspace's agents come to reach anything useful, and who decided they
could.

Nothing here is built. [egress.md](egress.md) describes the mechanism this
rests on and is mostly built; this is about who supplies the hosts and the
credentials, which that document deliberately does not answer.

## The deployment this is for

One entity runs the platform. Many entities have workspaces in it and use them
as part of their working day. Their own customers mostly never see outturn at
all.

A booking platform is the clearest example. The operator is the platform; a
workspace is a host with a few properties; the agents do booking, customer
service, billing, tax document preparation. The traveler books on the
operator's website and receives an email, and neither of those is served by
outturn. What outturn provides is the way a *workspace* works: agents that
reach the operator's booking API, the workspace's own tools, and whatever else
somebody has sanctioned.

That is worth stating because it settles a question the mockups left open. An
end-customer surface -- a guest signing in to talk to a concierge -- is a
different product with different auth and a different blast radius. It is not
what this is, and the row-level authorization it would need is not on this
path.

**The thing being defended against is a workspace's own agent.** Not a
malicious customer: the customer is not here. A workspace configures an agent,
writes skills for it, and that agent makes requests with credentials attached
by the gateway. The control point is therefore the workspace's configuration
and who may change it, not a permission shown to an end user.

## Three ways a host becomes reachable

Ordered by who decided, which is the only ordering that matters.

### 1. The operator installs their own APIs

The operator runs systems their workspaces need -- the booking API, payments,
whatever the business is. They allow those hosts, and workspaces get agents
that already know how to call them.

**This exists.** `egress_rules` carries the host and the name of the
environment variable holding a credential; `skill_version_hosts` lets a skill
declare the hosts it will reach; the difference between declared and allowed
surfaces as `unmet_hosts`. A workspace cannot grant itself a host.

What sits behind the declaration is `fetch_url` and a skill. A skill says it
will reach `api.example.com`, the rule permits it, and the agent composes the
calls from what the skill documents. That is the whole of an integration today,
and it is why this tier is the one that already works.

An API that issues its own access tokens rather than accepting a static key is
still this tier: the operator holds a client id and secret, and the gateway
exchanges them. [client-credentials.md](client-credentials.md) describes
that, which is built and stores nothing the gateway does not already hold.

### 2. Vetted extensions

A workspace wants their agents posting to Notion, or reading their Google
Calendar. They should be able to turn that on without the operator brokering
each one by hand, and without the operator having enumerated Notion in advance.

An extension is a package the operator has approved: the hosts it reaches, the
tools it exposes, the scopes it asks for. A workspace enables it and authorizes
it against their own account.

Two things make this the expensive tier, and neither is the packaging.

**Vetting is about scopes, not vendors.** "Notion is approved" is not a
statement anyone can act on. Posting a page and reading an entire workspace are
different permissions with different consequences, and an extension that asks
for the second while claiming the first is the whole risk. So an extension
declares the scopes it needs, the operator approves *that*, and a version
asking for more is a new approval rather than an update. This is the same shape
as `skill_version_hosts`: declared, never granted, and the declaration is
versioned so it cannot widen quietly.

**Some scopes reach further than the tenant.** OAuth bounds whose account acts,
not who is reached, and those are usually but not always the same thing.
Posting to the tenant's own Notion affects the tenant's own data, and a
misbehaving agent mostly hurts whoever enabled it. `gmail.send` on the tenant's
own Google account, through the same three-legged flow, will email anybody in
the world over the tenant's name. Calendar invites do the same; a chat vendor's
in-workspace write is bounded where its outgoing webhook is not.

So it is per-scope rather than per-vendor -- `gmail.readonly` and `gmail.send`
are the same vendor and the same flow -- and it is a minority of scopes on an
otherwise bounded catalog. It needs no separate mechanism, since approval is
already per-scope. What it needs is for the approval to be able to record that
a scope is a conduit, so the question gets asked when one is proposed rather
than noticed after something has been sent. A conduit scope may still be worth
approving; it is worth approving deliberately.

Constraining one means constraining the *arguments* -- a sender domain the
workspace proved it owns, recipients drawn from the operator's records -- which
an egress rule cannot express, because it checks the host and not the body.
That is a per-integration policy and belongs with the tool, which is another
reason the registration seam below is the load-bearing piece.

**The OAuth is three-legged, and that breaks an invariant.** Client credentials
-- machine to machine -- authenticate the platform to Notion, which is not what
is wanted: each workspace has their own Notion tenant and must authorize
outturn against it themselves. That means an authorization-code flow per
workspace, a refresh token per workspace, and refresh, revocation and
expiry handled by the platform.

**There is no single shape to store.** Three real providers, three lifecycles
-- one of which is refused outright, and is in the table because knowing why it
is refused is what keeps it from being adopted later for looking easy:

| | Grant | What is held | Lifecycle |
|---|---|---|---|
| Notion | authorization code | a token, per workspace | durable; nothing to refresh |
| Google | authorization code | a refresh token, per workspace | rotates on use, expires when idle |
| DocuSign | JWT bearer *(refused -- see below)* | one key, held by the platform | consent recorded per workspace; the key does not expire |

The JWT case is the one that breaks a naive design, because there is no
per-workspace secret at all. The tenant consents once against the platform's
integration key, and from then on the platform signs an assertion and exchanges
it for an access token. What the store holds for that workspace is the fact of
consent, not a credential.

**That shape is refused for anything a tenant uses.** One key that reaches
every tenant who has consented is a secret whose compromise is every tenant at
once -- a worse blast radius than anything else here, including
`OUTTURN_TOKEN_SECRET`, which at least only mints credentials for outturn. It
would also contradict the platform's one real promise: a compromise reaches
co-residency and nothing further. A credential that spans every workspace is
exactly the thing that does not.

So the rule is **one credential per workspace, always**, and it is a constraint
on which grants may be used rather than a preference about storage. A provider
offering only a platform-wide key is not integrated as a tenant-facing
extension, however convenient the lifecycle looks. Where a provider supports
both -- DocuSign supports authorization code as well as JWT bearer -- the
per-workspace grant is the one taken, and the operational cost of refresh
tokens is the price of the boundary holding.

The exception is a genuine platform integration: something the platform itself
uses, never on a tenant's behalf and never reachable from a workspace's agent.
There a single credential spans nothing, because there is nothing to span. The
test is not who pays for it or who benefits, but whether any workspace's
configuration can cause it to be used -- and if one can, it is not this case.

The rotating case breaks a different thing. A provider that returns a new
refresh token on each exchange invalidates the old one, so the new token must
be persisted in the same transaction that records the exchange. A process that
dies between receiving and storing has lost the grant, and the tenant must
authorize again. This is the most common way these integrations break in
practice.

Access tokens are refreshed on demand -- when one is needed and the held one
has expired -- rather than on a timer, with single-flight per workspace so
concurrent turns do not race. A timer buys nothing: a durable token needs no
refreshing, a sliding window is reset by ordinary use, and an absolute window
is not extended by refreshing early.

What does want a schedule is noticing a grant that has *died* -- revoked, or
past an absolute expiry. Not to keep anything alive, but so the workspace is
told their integration needs re-authorizing before an agent fails a turn with a
provider error the tenant cannot interpret. The constraint is the provider's;
discovering it from a broken turn is the platform's choice, and the wrong one.

Today credentials are named, never stored: `egress_rules.credential_env` holds
the name of an environment variable, and the property that falls out is that
nothing which reads that table can leak a secret by reading it -- which is why
the table is safe to return to a browser. A per-workspace refresh token cannot
be an environment variable. It is created at runtime, it rotates, and there is
one per workspace per extension.

So this tier needs a credential store that does not exist, and the naming
property has to be preserved some other way. The shape that keeps it: the store
is separate from `egress_rules`, a rule references a credential by id rather
than carrying it, and the gateway is the only tier that can dereference one.
Then the rules table stays safe to read and the secret lives in exactly one
place that is already the credential-holding tier. Encryption at rest and who
holds that key are settled in [Authorization-code grants](#authorization-code-grants)
below, and a static key a workspace pastes in -- the common case, which needs
no consent flow at all -- in [sealed-credentials.md](sealed-credentials.md).

Worth being honest that this is the tier with real work in it. Tier 1 is
configuration. Tier 3 is a flag. This is a token store, a refresh loop, a
consent flow and an approval model.

### 3. Workspace-allowed hosts, off by default

A workspace needs their agent to reach something nobody anticipated. The
operator has no reason to refuse and no wish to be asked.

**This is today's behavior, not a future feature.** `egress_rules` is
per-workspace and the workspace's list is checked first. So the work is not
adding the capability; it is adding the restriction, and the default flips from
what deployments do now.

A global setting, disabled by default, deciding whether a workspace may add its
own rules. Disabled means the table is operator-written and a workspace's
requests are refused unless a rule already exists for the host.

It should not be optional, however much it looks like the tier to drop. An
operator cannot enumerate every host every workspace will ever want, and
without this every integration becomes a support ticket with the operator as
the bottleneck. The default is off because a platform like the booking one
above wants it off; a deployment running a handful of trusted internal teams
wants it on and should not have to fork anything to get it.

What it does not waive is the address check. `resolve_and_vet` is not the
workspace's to bypass -- an allowed name that resolves inside the cluster is
still refused -- and the operator's internal-hosts list in
[egress.md](egress.md) remains the only way past that. A workspace permitted to
add hosts is permitted to add *public* hosts.

## A credential is bound to its host

Today a credential cannot be aimed anywhere. The gateway reads the named
environment variable and attaches the header
(`src/gateway/egress/mod.rs`), and both halves of that rule -- the host and the
variable naming the secret -- are set by an operator editing manifests. The
guest never holds the value, the runtime never holds it, and a request's own
headers are refused if they carry one.

Tier 2 breaks that arrangement by making the pairing workspace-editable. Once a
workspace holds a Notion token and can write egress rules, someone with
permission to edit a rule can point that credential at a host of their
choosing. Nothing is read, and the theft is complete anyway: the gateway
attaches the tenant's token to a request whose destination the attacker
controls.

So the defense is not keeping people from reading secrets, which already works.
It is that **a credential and the hosts it may travel to are one fact, fixed
when the integration is authorized, and not separately editable afterwards**. A
Notion credential attaches on Notion's hosts and nowhere else. The binding
belongs to the integration rather than to a row a workspace administrator can
change, and a rule that names a credential without inheriting that binding
should not be expressible.

For tier 1 the problem is already here: a rule names a variable, and the
variable namespace is shared by every workspace. How a variable is bound to its
workspaces and hosts, and why the binding lives on the gateway rather than in a
table or the turn token, is in [credential-bindings.md](credential-bindings.md),
which is built.

This is also why the authority to configure an integration is not the authority
to use one. A member who can enable Notion for a workspace is choosing what the
workspace's agents may do; the credential that results should not be reachable
by editing anything else.

## Which agents may use an integration

Enabling an integration for a workspace is not the same as offering it to every
agent in that workspace. A tax-preparation agent that can reach the tax
authority's API and nothing else is contained in a way that one holding every
credential the workspace owns is not -- and the containment matters precisely
because a model's output chooses the calls. A document containing an injected
instruction reaches whatever the agent could already reach.

`egress_rules` has `workspace_id` and no agent column, so this does not exist.
Adding it is a schema change and a resolution rule, and the resolution rule has
a trap in it: settings cascade operator to workspace to agent with an override
at each level, and egress must not. An agent's list may only *narrow* its
workspace's. An agent that could widen it would be a way out of the workspace
boundary, granted by whoever configures the agent -- which is the opposite of
what a per-agent list is for.

## What an integration actually is

A permitted host, a credential bound to it, and a skill saying what to call.
`fetch_url` already reaches any host a workspace is allowed, so nothing above
needs a new tool to become real -- the work is all in the rules and the
credential, which is where this document has spent its length.

That is worth stating because the opposite looked true for a while. Tools live
inside the guest component, declared in one list and dispatched by matching on
the name, so nothing above the sandbox boundary can add one. It reads like a
prerequisite. It is not: a generated `create_booking` tool would be checked by
exactly the same code as a `fetch_url` call to the same place, so it adds no
containment. The boundary is the egress rule and the credential the gateway
attaches, and both are already enforced.

What a typed tool would buy is argument shape -- a weaker model cannot malform
a request it did not compose -- and argument constraints, which is the only
mechanism that could pin a conduit's sender domain or bound its recipients.
Both real, neither a prerequisite. [roadmap.md](roadmap.md) records this as a
fork to take when a specific integration demands it.

An OpenAPI wizard is worth building either way. Pointed at a specification it
produces what a workspace needs to integrate: skill text today, tool
definitions if that turns out not to be enough, with only the consumer
changing.

## Authorization-code grants

The three-legged flow tier 2 rests on, designed and unbuilt: a person consents
at a provider, the provider redirects back with a code, and the platform holds
a refresh token from then on and trades it for access tokens the gateway
attaches. RFC 6749 section 4.1, with PKCE.

### Whose token it is

A grant is owned by a workspace or by one person within it, and the extension
declares which, per scope rather than per vendor. A shared Notion tenant is the
workspace's: whoever connected it was acting for the workspace, and their
leaving does not disconnect it. `gmail.send` is a person's: it sends over their
name, so it is theirs to give and theirs to take back, and nobody else's turn
may send as them.

On a turn with a `user_id`, a person-owned integration resolves to that
person's grant and nobody else's. If they have none the integration is
unavailable on that turn -- never another member's grant, and never a fallback
to a workspace grant of the same vendor, which would be acting as somebody the
person is not. A workspace-owned integration resolves the same way whoever is
typing.

A trigger turn has no `user_id`, deliberately -- it is what stops an agent
clearing its own stopped-session latch ([triggers.md](triggers.md)). It gets
workspace-owned grants as any turn does. It gets a person's grant only if the
trigger names it, and only the grant's own owner may name it, when they set
the trigger up or later: an explicit "this schedule sends as me", recorded on
the trigger and shown on it. Not inferred from the trigger's owner, because
owning a schedule for accountability and lending it your mailbox are different
things to have agreed to. A trigger whose named grant is revoked, or whose owner
has left the workspace, runs without it and says so, rather than finding some
other grant to use.

### Where the refresh token lives, and why that breaks a rule

**Credentials are named, never stored** cannot hold here, and this is where it
stops holding. A refresh token is minted at runtime, one per grant, and may
rotate on every use; there is no environment variable to name. So this is the
first secret the platform writes down, and the design is about keeping what the
rule was *for* -- nothing that reads a table can leak a secret by reading it --
after the rule itself is gone.

- **A table of its own, `oauth_grants`**, not columns on `egress_rules`. A rule
  references an integration; the integration resolves to a grant; only the
  gateway dereferences one. `egress_rules` stays safe to return to a browser.
- **Only the refresh token is stored.** Access tokens live in the gateway's
  memory, per replica, keyed by grant, for the reasons
  [client-credentials.md](client-credentials.md) gives for its own. A provider
  that issues a durable token and no refresh token (Notion) has that token
  stored in the same column, under the same treatment.
- **Encrypted with a key only the gateway holds.** `OUTTURN_GRANT_KEY`, gateway
  only, as `OUTTURN_TOKEN_SECRET` is API only. AES-256-GCM, a random nonce per
  write, and the grant's id, workspace and owner as associated data -- so a
  ciphertext copied onto another person's row fails to decrypt rather than
  attaching their token to the wrong turns. The ciphertext carries a key id, and
  the gateway accepts two keys during a rotation and re-encrypts under the newer
  one on the next refresh, the same shape as two public keys during a token
  rotation. Symmetric on purpose: only the gateway can make a ciphertext that
  opens, so a row proves the gateway wrote it. [sealed-credentials.md](sealed-credentials.md)
  seals a key a person pastes to a public key instead, which proves nothing
  about its author -- right for that case, and the reason the two are not one
  scheme.

What this keeps: the API, a backup, a read replica, a support query and anyone
holding the database password read ciphertext. What it does not: the gateway's
environment plus the table is every grant at once. That is the credential-holding
tier being what it already was -- it holds every provider key today -- and it is
why nothing else may hold `OUTTURN_GRANT_KEY`, the API included.

The rotation rule above applies with force. A provider that rotates on use has
invalidated the old refresh token by the time it answers, so the new ciphertext
is written in the statement that records the refresh, single-flight per grant
across replicas -- an advisory lock on the grant id, not a replica-local mutex,
because two replicas refreshing one rotating grant is one of them losing it.

### The redirect, state and PKCE

One callback for the deployment, `GET /v1/integrations/callback`, on the API
because that is the tier with a public name; one redirect URI to register with
each provider. Starting a flow is `POST /v1/integrations/{id}/authorize`, by a
signed-in person with the authority the integration's ownership needs --
`settings:update` for a workspace grant, membership for one's own. The API
records a pending authorization and returns the provider's URL:

- **`state`** is 32 random bytes, the key of a row holding workspace, person,
  integration, owner kind, the PKCE verifier and an expiry ten minutes out. It
  is consumed by `DELETE ... RETURNING`, so a second callback with the same
  value finds nothing.
- **PKCE with S256, always**, including for confidential clients where the RFC
  calls it optional. The verifier never leaves the server, so a code
  intercepted on the way back is worth nothing without the row.
- **The browser is bound too.** The start sets a short cookie holding a hash
  of the state, `Path=/v1/integrations/callback`, `SameSite=Lax` -- a top-level
  redirect from the provider carries it -- and the callback requires it. Without
  that, the attack is not CSRF in the usual direction but consent phishing:
  somebody starts a flow in their own workspace and sends the provider link to
  a victim, whose consent then lands as a grant the sender's agents can use.
  The state row says whose flow it was; the cookie says this browser started
  it. The refresh cookie cannot do this job, being scoped to its own path.
- **`iss`** is checked against the integration's issuer where the provider
  sends it (RFC 9207), so one provider's code cannot be replayed to another's
  token endpoint through a shared callback.

The exchange is the gateway's, because it carries the client secret and the
gateway holds credentials. The API forwards the code and verifier on an
internal call under a token of audience `outturn:gateway` and a role that holds
only the authority to complete an exchange -- not `Role::Turn`, so no turn token
can create a grant, and nothing a browser holds can reach it. The token
endpoint is vetted exactly as a client-credentials one is: resolved, pinned,
public unless allowlisted, https, no redirects. The gateway records the scopes
the provider actually granted beside the ciphertext, and the API sees the grant
id and those scopes, never the token.

Failure at the callback redirects to the UI with one of a fixed set of codes.
A provider's `error_description` is not shown, for the reason a token
endpoint's is not passed to a model: it is free text from somebody who has just
been handed a secret.

### What a turn's token commits to

The commitment already says which rules a turn may use; a grant-backed rule
gets its own leaf tag, as client credentials did, and the leaf adds the
integration and **the grant id the API resolved for this turn**. So the
question "whose token" is answered once, by the API, when the turn is minted,
and signed -- the gateway does not decide it again from a user id it would have
to trust the runtime about. A runtime holding a turn's token can use that turn's
grants and no others: rewriting the grant id fails the proof, and the grant's
own workspace and hosts are checked against the token's when it is loaded.

The hosts a grant may travel to come from the integration, not the rule, which
is the binding [above](#a-credential-is-bound-to-its-host) made concrete: a
rule that names a grant and a host the integration does not list fails before
anything is decrypted.

A trade-in at `POST /v1/work/{job_id}/token` copies commitments, as for every
other rule, so the grant a turn started with is the grant it keeps. That is
fine for which grant; it is not fine for whether the grant is still alive, which
is the next section.

### Revocation

Removing a host does not reach into a running turn. Revoking a grant must:
somebody who disconnects their Google expects it to stop sending now, not in
however many hours a long-horizon turn has left. So the commitment says which
grant, and the gateway asks whether it is still live on every use -- a cached
answer, invalidated over LISTEN/NOTIFY the way roles are, which also evicts the
access tokens held for it.

Who may revoke: the owner of a person's grant, and anyone with
`settings:update` for any grant in the workspace -- an administrator may cut
somebody's mailbox off from the workspace's agents without being able to use
it. Leaving a workspace revokes every grant that person holds in it.

Revoking wipes the ciphertext in the same statement that marks the row, then
calls the provider's revocation endpoint (RFC 7009) where there is one, best
effort. The order matters: a provider that is down must not leave the platform
holding a token it promised to forget. The row stays, without its secret, so
the ledger's attribution and the action queue still have something to point at.

A grant the provider revoked -- `invalid_grant` on refresh -- is marked dead
the same way, and the turn's agent reads the refusal table client credentials
uses: this credential could not be obtained, retrying will not help. The owner
is told, through the action queue, that it needs connecting again.

### Beside client credentials

[client-credentials.md](client-credentials.md) is a separate effort and stays
one. It is the operator's API authenticating as the platform, from two named
variables, storing nothing; it does not move into `oauth_grants`, and a grant is
not a client-credentials rule with a stored secret.

What they share is everything after the secret is in hand: the token endpoint
vetting, the in-memory access-token cache and its refresh-before-lapse rule,
single-flight, a 401 evicting without retrying, and the failure table. That
should be one module with two sources of the thing it exchanges.

The binding problem that document leaves open -- any workspace can name a
variable under `OUTTURN_EGRESS_` -- does not arise here, because a grant is
bound to a workspace and an owner by its key and its associated data, not by a
shared namespace. If client credentials later want the same binding, an
operator-written grant row holding an encrypted secret is the shape to borrow;
nothing in this design needs that to happen first.

## Open questions

- **Where a registered tool's definition comes from.** An OpenAPI document is
  the obvious source and is not the only one; whatever is chosen becomes a
  format the platform parses and therefore owns.
- **Whether a registered tool runs in the guest or the host.** In the guest it
  needs the definition to cross the sandbox boundary, which the interface has
  no shape for today. In the host it is not a tool the component can reason
  about at all, only one the host offers on its behalf. This is the load-bearing
  decision and it is not made here.
- **Whether extension scopes and egress hosts are one declaration or two.** A
  Notion extension names a host and a set of OAuth scopes; `skill_version_hosts`
  already names hosts. Merging them means one approval surface; keeping them
  apart means the host check stays as simple as it is.
- **What a workspace sees when an extension's new version asks for more.**
  Silently upgrading defeats the versioned approval; blocking until somebody
  looks means an integration stops working because nobody read an email.
