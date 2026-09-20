# Egress

Who may reach what, and why the runtime is not the one deciding.

## What exists

An agent makes no outbound request itself. It asks the gateway, which holds the
credentials, checks the workspace's rules, and makes the call -- so the tier
running workspace code never holds a key and never opens a socket to anywhere
but the three places it is allowed.

That is enforced twice, deliberately, in two tiers that cannot both be wrong in
the same way. In the code, by the runtime having no outbound HTTP path at all.
In the cluster, by a NetworkPolicy (`k8s/base/networkpolicy.yaml`) that lets the
runtime reach DNS, the API, the gateway and the object store, and nothing else
-- so a host that grew a socket still reaches nothing. Requires a CNI that
enforces NetworkPolicy; kind's default does not, which is fine locally and is
not something a deployment should assume.

A workspace allows hosts by name. A rule may carry a credential, which the
gateway attaches and the guest never sees; a rule that carries one may only be
reached over https, because a credential on a plaintext connection is a
credential given away.

And the check a workspace cannot waive: `resolve_and_vet` resolves the name and
refuses the request if the address is not plainly on the public internet. Every
private range, loopback, link-local, carrier-grade NAT. The list is of what is
*not* public rather than of the cluster's own addresses -- a list of ours would
need maintaining and would be wrong the first time something moved.

## Two different concerns, enforced in two places

Worth separating, because one mechanism looks like it is doing both jobs and is
not.

**The runtime must not reach outturn's own services.** That is the
NetworkPolicy's job, and it does it completely: the runtime's egress is an
allowlist of four destinations.

**The gateway must not be aimed at outturn's own services.** A workspace that
could allow `outturn-api` and ask the gateway to fetch from it would have turned
the gateway into a proxy into the cluster. That is `resolve_and_vet`'s job.

The private-range refusal is doing the second job, and doing it with a very
large hammer.

## The problem with refusing every private address

Deployments will want an agent to reach a service the customer runs. Most of
the time that service has a public name and the section below is irrelevant --
but some of them do not, and a ticketing API reachable only at
`tickets.internal` is refused today for the same reason `outturn-api` is: the
address is private.

So the guard is preventing the thing it exists to prevent *and* a case nobody
meant to forbid, and there is no way to have one without the other.

## First, the path that needs none of this

A customer's service usually has a public name. `tickets.acme.com`, resolving
to a public address, with a certificate. Reaching it needs nothing below: the
workspace allows the host, the address vets as public, the credential travels
over https, and it is the same path every other rule takes.

That is the recommended shape, and it is worth saying plainly because the
alternative below costs something. A credential to an internal host is
permitted over plain http -- internal services mostly do not terminate TLS --
so taking the internal route means a key crossing a network where other
tenants' agents are running. The public route has no such clause.

So the allowlist is an escape hatch, for a service that genuinely has no public
name and sits beside outturn in the cluster. That is a real case and a narrower
one than it first appears: a deployment on Kubernetes is not by itself a reason
to use it.

## An operator allowlist, by name

For the case the section above does not cover: a list of internal hosts the
*operator* has said the gateway may reach, checked before the private-address
refusal.

Operator-level and never workspace-settable. This is the whole point: a
workspace allowing `tickets.internal` is expressing what its agents need, and a
workspace allowing `outturn-api` is attacking the platform. Only somebody
outside the workspace can tell those apart, so only they may add to this list.
A workspace still has to allow the host in its own rules -- the operator's list
says a host is *reachable*, not that anyone may reach it.

Empty by default, so a deployment that has not thought about it behaves exactly
as it does now. `OUTTURN_INTERNAL_HOSTS` on the gateway, comma or whitespace
separated: configuration rather than a table, because an operator adding an
internal service is already editing manifests, and a table is what to build when
somebody wants the list without a redeploy.

**A name in a cluster is not one string.** `tickets`,
`tickets.default.svc.cluster.local` and `tickets.internal` may all reach the
same service, and which one a caller writes decides what the gateway is asked
for. A bare name is worse than ambiguous: the resolver's `search` list expands
it against the *caller's own* namespace, so the same string means different
things depending on where it is resolved from.

The list matches the host as the caller offered it, so an operator has to name
the spelling that will actually arrive. The fully-qualified form is the one to
write, being the only one that means the same thing everywhere.

Getting it wrong should not be a mystery, so a refusal says what it saw: the
host as offered, and that it was not on the list. That is the one line that
turns a silent refusal into a copyable answer, and it costs nothing -- the
gateway already has the string in hand.

Headless services and statefulset members are where this is least intuitive. A
headless service resolves to every pod behind it rather than one address, and a
member is `pod-0.svc.ns.svc.cluster.local`. Both work, and neither is what
somebody writing `tickets` expects to be matching.

**Names, not ranges.** An operator allowing `10.0.0.0/8` would re-open the path
to outturn's own services, and would not have meant to: it reads as "our
network", not as "including the API that mints tokens". A name is a deliberate
act about a specific service, and a service that moves keeps its name. The cost
is that an operator with forty internal services lists forty of them, which is
tedious and is also forty decisions somebody made on purpose.

An operator who adds outturn's own hostname to this list has built the proxy it
exists to prevent, and nothing here stops them. That is accepted rather than
solved: the list is the operator's statement of which services agents may call,
and naming the platform's own API in it is not a slip. A workspace still cannot
reach it -- the operator's list says a host is reachable, the workspace's rules
say who may reach it, and a guest would need the credentials and the context to
do anything with it. Several deliberate mistakes rather than one.

The alternative was a namespace: services an agent may reach get deployed into
a designated one, and a NetworkPolicy permits the gateway to reach that
namespace and no other. Kubernetes enforces it rather than a string comparison,
so no spelling of an internal name gets through. It was not chosen because it
costs operators a deployment convention to save them from a mistake they have
to make on purpose -- but it is the thing to reach for if this list ever needs
to be safe against its own holder.

## One list, both paths

There are two ways the gateway reaches a host, and only one of them is vetted.

An agent's request goes through `resolve_and_vet` and is refused if the address
is private. A model call does not: `Route::base_url` is used as given, so
`http://outturn-mockllm:8083` and an ollama at `172.18.0.1:11434` are reached
without anything asking whether they should be. That is how local development
works today.

So the same list answers both. A host an operator has named is reachable,
whether an agent asked for it or a route pointed at it; anything private that is
not on the list is refused on either path. That is the whole of the abstraction
paying for itself -- it is not a new policy beside an old one, it is one policy
where there were two, and one of those two was "no policy".

It also settles a question before it is asked. `traffic_routes` has a nullable
`workspace_id`, so a workspace's own routes are already a shape the schema
allows; nothing writes one today because routes are managed by hand. The moment
routes get an API, a workspace pointing one at `http://outturn-api:8080` is a
question somebody has to remember. Vetting the route path now means nobody has
to remember.

**Entries are hosts as written, names or literal addresses.** A literal address
is matched as itself and never resolved -- asking a resolver to look up
`172.18.0.1` would let it answer with something else. That means an address and
a name that resolves to it are separate entries, which is right rather than
tedious: allowlisting a name trusts DNS to keep pointing where you expect, and
allowlisting an address trusts nothing.

**An entry may name a port, and one that does permits only that port.**
`tickets.internal:8080` allows the ticketing API and not the Postgres beside it
on the same host, or the admin interface on 9000. The port is already resolved
before the check -- `port_or_known_default` runs a few lines above -- so matching
on it is one comparison, and narrowing a permission for one comparison is not a
trade worth thinking about.

A bare `tickets.internal` permits any port on that host. That is the looser
entry and it stays available, because an operator who wants the whole host
should be able to say so in one line rather than enumerating ports -- but a host
inside the network is a host running more than the service somebody meant, and
the narrower spelling is the one to reach for.

## Credentials to an internal host

A rule carrying a credential may only be reached over https, because a key on a
plaintext connection is a key given away. That is right for the public internet
and wrong for the case this feature exists for: an internal service is usually
plain http, TLS inside the cluster being a thing people mean to get to. An
authenticated call to `tickets.internal` would be refused -- and an internal API
wanting a key is more likely than one that does not, so the allowlist would ship
and fail on the first realistic service.

So a credential requires https **unless the host is one the operator
allowlisted**. The list is the discriminator, because it is the only exact one
available: the operator named that host, on a network they run, as somewhere
agents may call. Nothing is inferred from the shape of the name.

Not from the shape of the name, specifically. A bare hostname looks like it
marks the internal case and does the opposite -- the realistic spellings are
`tickets.internal` and `tickets.default.svc.cluster.local`, while `tickets` is
the one nobody uses because it only resolves from inside one namespace. A syntax
rule would select almost exactly the wrong set.

Worth stating plainly, because the allowlist now does two things rather than
one: it makes a private address reachable, *and* it permits a credential to
travel there in plaintext.

**Internal services should still terminate TLS**, and the reason is sharper than
"TLS is good". Runtime pods are shared between workspaces. A key crossing the
cluster in plaintext is a key on a network where other tenants' agents are
running, and the guarantee that they cannot read it rests on the network rather
than on anything outturn does. That is an argument for TLS the operator should
hear; it is not an argument for refusing to work without it, which would mean
every integration begins with a certificate.

## Not yet

- A route naming one of our own services by hostname is reached. Only a literal
  address is judged on the route path: resolving there was built and reverted,
  because a name that does not resolve costs the resolver's full timeout --
  four seconds, measured -- and it runs per route per request, so one stale
  route would stall every turn in the deployment. Narrow today, since routes
  have no write API and are written by whoever runs the deployment. The thing
  to do when routes become workspace-writable is give the gateway its siblings'
  names, not look them up.
- Where the list lives. Configuration on the gateway is the smallest thing that
  works and fits who may change it -- an operator adding an internal service is
  already editing manifests. A table like `egress_rules` is the shape people
  here already read, and is what to build when somebody wants to see the list
  without a redeploy. The check is the same either way; only the management
  surface differs.
