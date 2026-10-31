# User credentials

A connection that is the asking person's rather than the agent's, so that a
backend with permissions of its own sees who is asking and applies them.
Built but for machine users and the on-behalf-of header (*As built*, at the
end). It rests on [sealed-credentials.md](sealed-credentials.md)
and the commitment ([invariants.md](invariants.md), *The commitment is what
makes a rule a rule*), both built, and generalizes the ownership rules that
[integrations.md](integrations.md#whose-token-it-is) wrote for OAuth grants to
every credential the gateway attaches.

## The gap

A skill's connection is one credential for the whole workspace. Every turn of
every agent that uses it reaches the backend as the same caller, so the backend
sees "the integration" and nothing about the person behind the turn.

Between companies that is fine, and stays fine: each workspace seals its own
key, bound to its own id, and nothing here changes that. The gap is inside a
company. An operator hosts a bookkeeping service; its customers' employees talk
to an agent; one of them asks "show me my ten most recent invoices". The
service has its own idea of who may see which invoices -- a clerk sees their
department's, a manager sees everybody's -- and it never gets to apply it,
because the request arrives under a key that can see all of them. Today the
only way to keep the clerk from the manager's invoices is to put that rule in
the agent's prompt, which is to say not to keep it.

The backend already has the right answer. What it lacks is the right caller.

## Two owners

**A connection is either the agent's or the asker's.**

- **The agent's**, as now. One credential, named by the rule, attached on every
  turn whoever is typing. Right for an API the agent uses on the workspace's
  behalf: a booking API, a shared ledger, a spreadsheet shared with a service
  account.
- **The asker's.** Each person connects their own credential, and a turn uses
  the one belonging to whoever asked. The asker is the person who sent the
  prompt the turn answers: the `user_id` on that `agent_messages` row and on
  the job's payload, which the platform already records and already keeps null
  for anything it produced itself.

Ownership is a property of the rule, `owner: agent | asker`, set by whoever
holds `settings:update`, beside the host and header it already carries. Not of
the skill: a skill declares the hosts it reaches and the header its API
expects and has never named a credential, and one skill published to every
client may be the agent's connection in one workspace and the asker's in
another. Not chosen per turn either, and never by the agent. The agent calls
`fetch_url` exactly as it does today; which credential goes with the request
is the platform's decision, made before the turn starts, and the guest has no
way to state a preference.

That last point is the reason for the design rather than a detail of it. Many
people talk to one agent, sometimes in one session. If the agent chose whose
credential to use, a prompt injected into a fetched page could choose too.
So the system threads the asker's credential through, the gateway attaches it,
and the runtime never holds it -- the same path every credential already takes.

## Connecting one

A person connects their own credential on their Account page, under
*Connections*, which lists every asker-owned rule in the workspaces they
belong to. It is a sealed credential like any other: sealed in the browser to
the gateway's public key, its binding as associated data, stored in
`credentials`. The binding gains one field:

```json
{
  "credential": "0192…",
  "kind": "static",
  "workspaces": ["0192…-acme"],
  "hosts": ["books.example.com"],
  "token_url": null,
  "header": "Authorization",
  "user": "0192…-dana"
}
```

**`user`** is the person the credential belongs to, required for a credential
an asker-owned rule uses and refused on one an agent-owned rule names.
`workspaces` is that person's one workspace, never `"*"`: a key a person
connected in ACME is theirs in ACME, and their membership of another workspace
is a separate fact with its own connection. The row gains a `user_id` column
for indexing, and the binding is the truth, as it is for everything else in
the row.

Inside the seal for the reason the rest of the binding is. A database writer
who could move Dana's key onto Sam by editing a column would have Sam's turns
working as Dana; with `user` in the associated data, the edited row does not
open. The provenance gap [sealed-credentials.md](sealed-credentials.md#who-can-seal-and-what-that-allows)
describes is unchanged -- anybody can seal, so a database writer can still
seal their own key as Dana's -- and is closed the same way: the fingerprint,
shown on Dana's Account page beside the connection, is computed by the gateway
and changes when the key underneath does.

**Who may store one: the person, and nobody else.** Storing a credential as
somebody is acting as them. An administrator with `credentials:write` may not
connect a key under Dana's name, however helpful it would be at onboarding,
for the reason [integrations.md](integrations.md#whose-token-it-is) gives about
a person's grant: it is theirs to give and theirs to take back. Connecting
one's own needs no authority beyond membership, as one's own account needs
none; the API refuses a binding whose `user` is not the caller.

**Who may revoke: the person, and anyone with `settings:update`.** An
administrator may cut somebody's connection off from the workspace's agents
without being able to use it. Leaving a workspace revokes every credential the
person connected in it. Revocation is what [sealed-credentials.md](sealed-credentials.md#revocation-reaches-a-running-turn)
already describes -- the row marked, the ciphertext wiped, a notification --
and reaches a running turn the same way, because the gateway reads the row on
use.

Kinds carry over as they are. A `static` key is the case this is for. A
`service_account` belonging to a person makes little sense -- a service account
exists to not be a person -- and is refused for an asker-owned rule. A client
pair is the platform authenticating as itself and is the agent's by nature.

## Whose turn it is

**The API decides, once, when it mints the turn, and signs what it decided.**
The gateway never works out whose credential to use from a user id it would
have to trust the runtime about. That is the shape integrations.md chose for
grants, and it applies here unchanged.

When the worker prepares a turn, it resolves each asker-owned rule against the
job's `user_id` to one credential id, or to nothing. The rule the runtime
carries names that credential id, as an agent-owned sealed rule does today, and
the commitment covers it: an asker-owned rule gets a leaf tag of its own,
covering the rule, the resolved credential id and the asker. A runtime that
swapped Sam's credential id into Dana's turn fails the proof, as a runtime
swapping any credential already does.

The turn token gains one claim, `asker`: the user id, or absent on a turn
nobody sent. It is not how the gateway picks a credential -- the leaf is -- but
it is the second check that has to agree. The gateway opens the credential the
leaf names and refuses it unless the binding's `user` equals the token's
`asker`. Two signed statements from the API and one sealed by the person who
held the key: a forged row fails the seal, a rewritten rule fails the proof,
and a credential sealed for somebody else fails the comparison. The claim is
also what the on-behalf-of header below asserts, and what the usage ledger
can record against a request where today it records only the session.

The turn token's subject stays the chat session. Changing it to the person
would make it read like a browser token, which [invariants.md](invariants.md)
(*A token is good for one audience*) is at pains to keep apart, and would
leave a trigger turn with no subject at all.

A trade-in at `POST /v1/work/{job_id}/token` copies the commitments and the
`asker` claim from the presented token rather than recomputing them, as it
copies everything else. The credential a turn started with is the credential
it keeps; whether that credential is still alive is the row's question, asked
on every use.

## Several people in one session

Each turn has one asker, so each turn uses one person's credentials. When Dana
asks and then Sam asks in the same session, Dana's turn reaches the backend as
Dana and Sam's as Sam. Sam's turns never carry Dana's credential, and that is
the guarantee: nobody can ask the backend for anything with somebody else's
access.

What this does not do is make the conversation private, and it is worth being
plain about, because the obvious extension of the rule fails. A session is one
conversation that everyone in it reads, and the model reads it too. Dana's
invoices, fetched with Dana's key, are in the tool results of her turn and in
the reply the agent wrote from them, and Sam's turn sees both. Withholding the
tool results from Sam's turn would leave the reply that quotes them, and
withholding the reply would leave Sam reading a conversation with holes in it
that the model is still being asked to continue. Neither is a boundary.

So the boundary is the one that already exists: bringing what your credential
reaches into a shared session is sharing it with that session, exactly as
pasting it would be. The platform's part is to make that visible rather than to
pretend it away:

- every call made with an asker's credential is drawn with whose it was --
  *with Dana's connection* -- in the transcript, for everyone reading it;
- the first time a person's credential is about to be used in a session that
  has other participants, the turn says so before it runs, once per person per
  session.

Reading a session stays `sessions:read`, as now. A workspace for which that is
not enough wants sessions that only their creator can read, which is a
question about sessions, and is recorded under *Not settled*.

## Nobody asked

A turn with no person -- a schedule, a webhook, mail, an automation -- has no
asker, and an asker-owned rule then has one of three answers, chosen on the
rule by whoever holds `settings:update`:

- **The agent's credential.** The rule names one, used only when nobody asked.
  Right for a backend that has an integration account for exactly this.
- **A named machine user.** An account in the workspace that cannot sign in,
  exists to hold connections, and is named on the rule; an administrator
  connects its credential, which is the one exception to *nobody stores a key
  as somebody else* and the reason the account cannot sign in -- it is nobody.
  The turn's `asker` is the machine user, so the binding check and the
  on-behalf-of header work unchanged, and the backend sees a caller it can
  give its own narrow permissions to.
- **Refused**, the default. The rule resolves to nothing, as for an asker with
  nothing connected, below.

Never a person's credential by inference. A trigger's owner set it up and is
accountable for it; that is not the same as having agreed to lend it their
access, and [integrations.md](integrations.md#whose-token-it-is) already draws
this line for grants. A trigger that should run as a person names their
credential explicitly, only that person may name it, and it is shown on the
trigger -- the rule integrations.md wrote, which applies here as it stands.

The `user_id` on a trigger's prompt stays null whatever the rule chooses.
`asker` is a claim on the turn token about whose credentials the turn carries,
not a statement that a person sent it, and the stopped-session latch
([triggers.md](triggers.md)) goes on reading the prompt.

## An asker with nothing connected

The rule resolves to nothing, and that is committed as its own answer: the
leaf says *this asker has no credential for this rule*, so the gateway refuses
the request without sending it and says why, in words the agent can relay:

> Dana has not connected books.example.com. They can connect it under
> Account → Connections; nothing was sent.

**Never a silent fallback.** Not to the agent's credential, not to another
member's, and not by sending the request bare and letting the backend answer
401. Each of those either acts as somebody the person is not, or turns a
missing connection into a puzzling failure from a backend. The person learns
why, and the agent can tell them where to fix it.

The refusal is the floor rather than the whole of it. The page shows the asker
which of an agent's connections are theirs to make before they start, and a
turn whose asker is missing one is told so in that turn's prompt -- not the
system prompt, which [prompt-contributors.md](prompt-contributors.md) composes
once per conversation, and so cannot hold anything that differs by who is
typing.

## An approval answered by somebody else

[approvals.md](approvals.md) holds a gated request and sends it once somebody
answers. The person who answers is often not the person who asked: Dana asks
for a refund, Lee approves it. **The held request is sent with Dana's
credential.**

The approver's yes is about the act -- this refund, of this amount, may go. It
is not a loan of Lee's access, and Lee holding `approvals:answer` says nothing
about what Lee may do in the backend. Sending with Lee's credential would have
the backend apply Lee's permissions to Dana's request, which is the gap this
document exists to close, reopened by the approval step. Sending with Dana's
means the backend still decides whether Dana may issue refunds; an approval
adds a person's yes on top of the backend's own check and never stands in for
it.

So the held request records the credential the refused request resolved to,
and the asker, beside the bytes. That matters more than it looks, because a
held request outlives its turn: approvals.md resends an approved request on the
*next* turn when the first could not send it, and the next turn may be Sam's.
The credential is the held request's, not the carrying turn's. When the API
mints a turn that will send a held request, the commitment carries one more
leaf, binding the held request's id to its credential and asker, and the
gateway uses it for that request alone.

If Dana's credential is revoked, or Dana has left, between the ask and the
answer, the held request is *never sent*, and the item reopens as approved but
not sendable, saying why. Not sent with whoever's credential is to hand.

## Per-user OAuth, later

An OAuth grant a person owns -- integrations.md's authorization-code flow,
unbuilt -- is an asker's credential obtained another way. Everything above holds
for it: the rule is asker-owned, the API resolves the asker's grant at mint
time, the leaf names it, the gateway checks its owner against `asker`, and an
asker with no grant is refused and told. integrations.md's *Whose token it is*
and this document describe one rule, and should be built as one:
resolution is over credentials, and a sealed key and a grant are two rows a
credential id can point to.

[service-accounts.md](service-accounts.md) already relies on the gateway
resolving a credential to an access token, independently of how it was
obtained, so a skill does not change when its credential becomes a person's
grant. That is what lets an operator start with each employee pasting an API
key and move to a consent flow later without republishing anything.

## A signed on-behalf-of header, instead

Some backends would rather trust the platform than hold a credential per
person: the operator's own service, which knows outturn, has its own users,
and would take outturn's word for which of them is asking. For those, a
credential per person is friction -- every employee pastes a key they had to
go and make -- and the alternative is one credential and a signed statement.

The rule is agent-owned and carries `on_behalf_of: true`, covered by its leaf.
On each request the gateway attaches the agent's credential as now, and an
`Outturn-On-Behalf-Of` header holding a JWT it signs:

- `iss` the deployment, `aud` the host the request is going to, as bound;
- `sub` the token's `asker`, and `workspace` its workspace;
- `iat`, an `exp` a minute out, and a `jti`, so a header logged by the backend
  is worth little and worth nothing elsewhere.

**Signed by the gateway, not the API.** The gateway makes the request and
knows the host, so the assertion can be for that host and that minute. The API
mints once per turn, for thirty minutes, before anyone knows where the turn
will go; an assertion minted there would be good for every host for half an
hour. So the gateway holds a signing key of its own, `OUTTURN_ASSERTION_KEY`,
gateway only, and its public half is published at a well-known JWKS path on
the API, the tier with a public name. The runtime still signs nothing.

The header is attached only on a rule that asks for it, only toward the
binding's hosts, and only with a credential beside it: the agent's credential
says *this is outturn*, the header says *for this person*, and a backend should
accept the second only from a caller that proved the first. A turn with no
asker sends no header, or the machine user's, by the rule's *Nobody asked*
answer -- never a header with the subject left out, which a backend could read
as the integration itself.

What the backend does with `sub` is the backend's. outturn's user id means
nothing to it until somebody maps it, and that mapping is the work this
alternative moves to the backend: an account-linking step on its side, or an
external id the workspace sets per person, which is the shape
[authorities.md](authorities.md#membership-is-a-separate-question) anticipates
for identity providers. An email claim would be easier and is left out until
the platform verifies addresses, because a backend matching on an unverified
email is a backend anyone can be.

Which one an operator picks is about who holds the permission. A key per person
leaves the backend proving who is asking; the header leaves it trusting outturn
to have checked. The first is right for a backend outturn does not control, the
second for one where the operator is on both sides.

## What this does not change

- **Tenancy.** Workspaces' keys are their own, bound to their ids, as they are.
- **Egress.** Which hosts a turn may reach is the workspace's rules; an asker's
  credential attaches only toward the hosts its rule and its binding both name.
- **The runtime.** It carries a rule naming a credential id and relays a proof,
  as now. It learns the id of Dana's credential and never its contents.
- **Approvals' gates.** A gate fires on the request whoever's credential it
  carries; a person's own key is no reason to skip a gate the skill declared.

## Order of work

1. Core: `user` on the binding, parsed and refused as strictly as the rest; a
   leaf tag for an asker-owned rule covering the resolved credential and the
   asker, and one for *nothing connected*; the `asker` claim on the turn token,
   copied on a trade-in. Tests that a credential sealed for one person is
   refused on another's turn, that an edited `user` does not open, and that a
   leaf naming another person's credential fails the proof.
2. Migration: `credentials.user_id`, `egress_rules.owner` and the rule's
   *nobody asked* answer, `held_requests` gaining credential and asker. Machine
   users as a kind of account that cannot sign in.
3. API: resolving asker-owned rules at mint time; the Account page's
   connections, stored only by their owner, revocable by the owner and by
   `settings:update`, revoked on leaving; the held request's own leaf.
4. Gateway: the binding check against `asker`, and the refusal for nothing
   connected.
5. The page: whose connection each call used, the one-time notice in a session
   with other participants, and which connections an asker is missing.
6. On-behalf-of: the assertion key, the header and the JWKS, behind the rule's
   flag. Independent of 3 to 5, and the smaller of the two for a backend that
   can take it.

A rule gains a leaf tag that travels from the API through the runtime to the
gateway, so the three tiers deploy together, as they did for sealed rules.

## As built

Steps 1 to 5 but for machine users and the one-time notice in a shared
session.

- `egress::seal`: the binding's `user`, refused on any kind but `static` and
  with any `workspaces` but one id; `Binding::belongs_to`, which the gateway
  asks beside `allows`.
- `egress::rule`: `EgressRule::asker`, and in `egress::commit` its own leaf
  tag over the person and the credential or its absence.
- `auth::token`: the `asker` claim (`ask`), taken by `mint_turn` and copied
  on a trade-in at `POST /v1/work/{job_id}/token` -- only after it is checked
  to be the leased job's own (`work::asker_of`, the one answer the mint asks
  too), so a person's token kept past their turn cannot be renewed under
  somebody else's.
- `gateway::egress`: a rule's `asker` compared with the token's, nothing
  connected refused and said, a person's seal refused anywhere but on their
  own turn -- by `Binding::attaches`, the same check the API runs when a rule
  is written, so an agent's rule naming a person's key is refused there too.
- `api::credentials`: a binding naming a person is refused on the workspace's
  routes; only their own connection stores one.
- Migration `0074_user_credentials.sql`: `egress_rules.owner`, `agent` or
  `asker`; `credentials.user_id` and `rule_id`, one live key per person per
  rule; names unique only among the workspace's own keys, since a person's is
  named for its host; and a trigger revoking a person's keys when their last
  role in the workspace goes, however it goes.
- `api::egress`: a rule written with `owner: asker`, which takes a header and
  no environment variable, exchange or wildcard, and may name the workspace's
  credential for a turn nobody sent. Its header cannot change afterwards,
  since people's keys are sealed to it. `rules_for_turn` resolves each
  asker's rule for the prompt's `user_id`; `prepare_turn` and `gates_for` both
  use it, so the gates the API later derives again are over the same hosts.
- `work::asker_of` is the prompt's `user_id`. A trade-in still renews a token
  with no asker, which carries nobody's key -- a turn minted before this,
  across a deploy.
- `api::connections`: `GET /v1/connections` lists the caller's connections
  with the seal key to seal to, since connecting one's own needs no
  authority; `PUT /v1/connections/{rule_id}` connects or replaces a key sealed
  as the caller's, for this workspace, host and header; `DELETE` disconnects
  it. Members only -- a role, not just a token -- so leaving reaches every key.
  An administrator revokes one through `DELETE /v1/credentials/{id}`.
- `api::egress::set_owner`, `PUT /v1/egress-rules/{id}/owner`: a host's
  connection made each person's own, keeping or gaining the header their keys
  travel in, or one key for everyone again, which revokes the keys people
  connected for it. `settings:update` and `credentials:write`, as connecting
  the workspace's key is.
- The page. Settings -> Connections and a skill's Connections say when a
  host's connection is each person's own, switch it either way, and connect
  the workspace's key for turns nobody started, in the header that is fixed.
  The Account page lists *Your connections* -- only when the workspace has
  such a host -- and connects, replaces and disconnects the person's own key,
  sealed in the browser with their id in the binding
  (`ui/src/components/OwnConnections.tsx`). People's own keys are left out of
  *Keys no host uses*.
- `held_requests.asker`, written from the refusing turn's `user_id`. A held
  request for a host whose rule is an asker's is handed only to a turn of that
  person, and any other turn in the session leaves it waiting -- every turn
  used to send the session's waiting requests, which with an asker's rule
  would have sent Dana's with Sam's key, or the workspace's on a resend.
- `tests/api/credentials.rs`, `a_persons_credential_goes_only_with_their_turns`
  and `a_person_connects_their_own_key_and_their_turns_carry_it`;
  `tests/api/action_queue.rs`, `a_held_request_to_an_askers_host_waits_for_their_turn`.

Two things the build decided that the design left open. On a turn nobody sent,
an asker's rule naming no workspace credential is left out of the turn's rules
rather than committed as *nothing connected*: there is no asker for that leaf
to name, so the host is simply not reachable on that turn. And a held request
is not given a leaf of its own: it waits for a turn of its asker's and goes
under that turn's rules, which are already resolved for them. The cost is that
*Send again* on such a request, being nobody's turn, leaves it for the asker's
next one.

Not built: machine users, the transcript saying whose connection each call
used and the notice before a person's key is first used in a shared session,
and the on-behalf-of header.

## Not settled

- **Sessions only their creator can read.** The honest answer to *Several
  people in one session* above, and the one a company that wants per-person
  privacy will ask for. A question about sessions and `sessions:read`, not
  about credentials.
- **Per-agent ownership.** Egress rules are the workspace's, so a host's
  connection is the asker's for every agent in the workspace or for none.
  Per-agent egress ([integrations.md](integrations.md#which-agents-may-use-an-integration))
  is where that would narrow.
- **Mapping a person to a backend's user** for on-behalf-of, beyond the
  platform's own id: an external id per person, set by hand or by an identity
  provider, and whether it travels as its own claim.
- **Which agent's connections a person sees.** The Account page lists every
  asker-owned rule in their workspaces; whether a person should see rules for
  agents they cannot talk to is not decided.
