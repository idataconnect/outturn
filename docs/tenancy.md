# Tenancy, credentials and attribution

Three questions that turn out to be one: who is isolated from whom, whose key
pays, and who gets the bill.

## Two levels, not a tree

An operator runs a deployment. Their customers each need isolation from one
another — nobody wants their operating procedures reachable by a competitor
using the same platform. And those customers may in turn have customers.

That reads like arbitrary nesting and should not be built as one. A parent
pointer with unbounded depth makes every authority check walk an ancestry,
makes isolation a question about descendants rather than a column comparison,
and makes storage prefixes carry a whole path. It also ruins the semantic index
before it is written: a pgvector partition on a fixed column is fast and dull,
and one on an arbitrary tree depth is neither.

So: two levels, and the isolation boundary is the one that already exists.

- **Tenant** — the isolation boundary. Storage prefix, egress rules, traffic
  routes, sessions, and the partition key for anything indexed. This is what
  `tenant_id` already means everywhere in the code, and it does not change.
- **Organization** — owns tenants. Billed, administers, and may see across the
  tenants it owns by authority rather than by sharing a boundary with them.

A customer with their own customers gives each of them a tenant. Isolation is
then the same enforcement already in place, keyed on the same column, and
`scope::resolve` stays four lines.

The change is additive: an owner on `tenants`, and an authority for
administering an organization's tenants. Nothing in storage scoping, egress or
routing is touched.

Where a tenant is itself too large to be one boundary — a customer wanting
their own departments isolated from each other — that is a partitioning
conversation for a particular deployment, not a shape the core product should
carry for everyone.

## Whose key pays

Three arrangements are wanted, and they are one mechanism rather than three:

- The organization holds a key and its tenants use it. The organization
  absorbs the cost and bills its tenants by whatever arrangement they have.
- Tenants bring their own keys and are billed by the provider directly. The
  organization offers nothing.
- Both: the organization's key is the default, and a tenant that has its own
  uses that instead.

All three fall out of one lookup and one switch. Resolution tries the tenant's
credential first and falls back to the organization's; the switch is whether
the organization offers its credential downward at all. With it off, a tenant
without its own key cannot run — which is the second arrangement, and is a
refusal rather than a surprise bill.

This fits what exists. `traffic_routes.credential_ref` already names a secret
rather than holding one, so what changes is which ref is chosen, not how
credentials are stored or reached.

## Defaults, and who gets to set them

An agent's policy decides which model runs its turns, which route serves them,
whether the model deliberates before answering, and how many tool rounds a turn
may take. Today it is per-agent and nothing above it has an opinion, so every
agent is configured from scratch by whoever made it — and an agent created
without one runs on whatever the deployment happens to default to.

That is the wrong level for most of these. Whether a local model should think
before answering is a property of the deployment, not of an agent: the site
owner knows they are running an 8B model at ten tokens a second, and no tenant
should have to discover that half a minute of silence is deliberation rather
than a hang.

The shape is the same as credential resolution, and should share its
machinery: a value resolved from the most specific place that sets it, with
each level able to leave it unset rather than being forced to choose. Agent,
then tenant, then organization, then whatever the deployment ships with.

The switch matters as much as the default. Some settings an operator wants to
suggest and let a tenant override; others they need to impose — a model
allowlist, a ceiling on tool rounds, an egress posture. That is the same
distinction the credential design draws between offering a key downward and
requiring tenants to bring their own, and it wants one mechanism rather than
two.

## Attribution

Two different questions, and today only the first is answered.

**Where did the work happen?** The tenant. Recorded: every message carries
`session_id`, `user_id`, `provider`, `model`, and five token counts —
prompt, completion, cache read, cache write and reasoning.

**Whose credential paid for it?** Not recorded. When an organization's key
serves a tenant's work, those are different parties, and the difference is
exactly what a bill has to be decomposed by. Without it an organization
absorbing spend across fifty tenants can see a total and cannot break it down,
and a tenant on its own key cannot demonstrate which charges were theirs.

The fix is one nullable column naming the credential that served the turn.
It is known at the moment the route is resolved, so it costs a field rather
than any new machinery, and it makes both questions answerable from the same
row: which tenant ran it, and which key paid.

Worth recording the resolution as it happened rather than inferring it later.
A tenant that adds its own key on Tuesday must not appear to have paid for
Monday, and a rule read at query time would say exactly that.

## Not built

Organizations, the ownership column, the credential resolution order and its
switch, and the paying-credential attribution. `tenant_id` is the isolation
boundary today and will remain so; everything here is added above and beside
it rather than through it.
