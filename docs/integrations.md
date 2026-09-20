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
service, billing, tax document preparation. The traveller books on the
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

### 2. Vetted extensions

A workspace wants their agents posting to Notion, or reading their Google
Calendar. They should be able to turn that on without the operator brokering
each one by hand, and without the operator having enumerated Notion in advance.

An extension is a package the operator has approved: the hosts it reaches, the
tools it exposes, the scopes it asks for. A workspace enables it and authorises
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
otherwise bounded catalogue. It needs no separate mechanism, since approval is
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
is wanted: each workspace has their own Notion tenant and must authorise
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
authorise again. This is the most common way these integrations break in
practice.

Access tokens are refreshed on demand -- when one is needed and the held one
has expired -- rather than on a timer, with single-flight per workspace so
concurrent turns do not race. A timer buys nothing: a durable token needs no
refreshing, a sliding window is reset by ordinary use, and an absolute window
is not extended by refreshing early.

What does want a schedule is noticing a grant that has *died* -- revoked, or
past an absolute expiry. Not to keep anything alive, but so the workspace is
told their integration needs re-authorising before an agent fails a turn with a
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
holds that key is a decision this document does not make.

Worth being honest that this is the tier with real work in it. Tier 1 is
configuration. Tier 3 is a flag. This is a token store, a refresh loop, a
consent flow and an approval model.

### 3. Workspace-allowed hosts, off by default

A workspace needs their agent to reach something nobody anticipated. The
operator has no reason to refuse and no wish to be asked.

**This is today's behaviour, not a future feature.** `egress_rules` is
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

So the defence is not keeping people from reading secrets, which already works.
It is that **a credential and the hosts it may travel to are one fact, fixed
when the integration is authorised, and not separately editable afterwards**. A
Notion credential attaches on Notion's hosts and nowhere else. The binding
belongs to the integration rather than to a row a workspace administrator can
change, and a rule that names a credential without inheriting that binding
should not be expressible.

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
