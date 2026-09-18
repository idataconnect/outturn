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

Most deployments will want an agent to reach a service the customer runs, and
most of the time that service runs in the same cluster. A ticketing API at
`tickets.internal`, an inventory service, whatever the workspace's work is
actually about. Today all of it is refused, for the same reason `outturn-api`
is: the address is private.

So the guard is preventing the thing it exists to prevent *and* the ordinary
case, and there is no way to have one without the other.

## An operator allowlist, by name

The fix is a list of internal hosts the *operator* has said the gateway may
reach, checked before the private-address refusal.

Operator-level and never workspace-settable. This is the whole point: a
workspace allowing `tickets.internal` is expressing what its agents need, and a
workspace allowing `outturn-api` is attacking the platform. Only somebody
outside the workspace can tell those apart, so only they may add to this list.
A workspace still has to allow the host in its own rules -- the operator's list
says a host is *reachable*, not that anyone may reach it.

Empty by default, so a deployment that has not thought about it behaves exactly
as it does now.

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

- The allowlist itself. Everything above is design.
- Whether a name is enough, or a name and a port. A customer running two
  services on one host would want the second; nobody has yet.
