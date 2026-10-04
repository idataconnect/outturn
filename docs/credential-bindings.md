# Credential bindings

Which workspace may use a credential the operator set up, and where it may be
sent. Built: `src/egress/bindings.rs`, enforced in `src/gateway/egress/mod.rs`.

Designed to be replaced: [sealed-credentials.md](sealed-credentials.md) keeps
the property this document argues for -- nobody who can write the database can
change where a secret goes -- while moving credentials out of the environment,
so adding one stops being a pod roll. The rule and the checks below carry over
unchanged; what moves is where the binding is held, from a variable beside the
secret to associated data sealed with it.

## The leak

A rule names the variable its credential comes from -- `credential_env`, or
`client_id_env` and `client_secret_env` for a client-credentials exchange --
and the gateway reads it from its own environment. Rules are written by a
workspace, under `settings:update`, which a workspace can grant itself. The
`OUTTURN_EGRESS_` prefix keeps the operator's own secrets out of reach, and
nothing more: the namespace is shared by every workspace.

So workspace B writes a rule for `collect.b-example.com` naming
`OUTTURN_EGRESS_ACME_STRIPE_KEY`, a variable the operator set up for ACME, and
B's agent fetches from that host. The gateway attaches ACME's Stripe key on the
way out. With client credentials it is worse by one step: the rule names the
token URL as well, and the gateway sends the client secret itself to it on the
first exchange. Nobody read anything; the secret was delivered.

This is the binding [integrations.md](integrations.md) asks for -- "a
credential and the hosts it may travel to are one fact" -- arriving early,
because tier 1 already has the problem it described for tier 2.

## The rule

**A credential variable is used only for the workspaces it was bound to, and
only toward the hosts it was bound to.** For a client-credentials pair, the
token URL is bound too: the secret goes to that URL and nowhere else, and the
token it buys goes to the bound hosts.

A rule may still *name* any variable under the prefix. Naming one it is not
bound to gets it nothing: the gateway refuses the request rather than sending
it without the credential, since a request the rule's author expected to be
authenticated going out bare is a surprise nobody wants to debug.

Unbound means refused. A variable with no binding attaches to nothing, for
anybody -- the same direction as the commitment, where "could not verify" has
to mean refused.

So does a declaration that cannot be read. If `OUTTURN_CREDENTIAL_BINDINGS` is
not valid JSON, or any entry in it is wrong -- an unknown field, a wildcard
host, a workspace that is not an id -- the whole of it is refused and nothing
is bound, and the gateway says so at error level when it starts. Not entry by
entry, as the internal-hosts list is read: dropping a bad internal host leaves
that host refused, but dropping a misspelt `token_url` from a binding would
leave the rest of it bound to its hosts with no endpoint said. A parse error
never falls back to allowing, and refusing to start instead would turn a typo
into an outage of every model call too.

## Where a binding lives

On the gateway, beside the secret it binds. One variable,
`OUTTURN_CREDENTIAL_BINDINGS`, holding JSON:

```json
{
  "OUTTURN_EGRESS_ACME_STRIPE_KEY": {
    "workspaces": ["0192…-acme"],
    "hosts": ["api.stripe.com"]
  },
  "OUTTURN_EGRESS_ACME_LEDGER_ID": {
    "workspaces": ["0192…-acme"],
    "hosts": ["ledger.acme.example"],
    "token_url": "https://auth.acme.example/oauth/token"
  },
  "OUTTURN_EGRESS_ACME_LEDGER_SECRET": {
    "workspaces": ["0192…-acme"],
    "hosts": ["ledger.acme.example"],
    "token_url": "https://auth.acme.example/oauth/token"
  }
}
```

Deliberately *not* under the `OUTTURN_EGRESS_` prefix: a rule could otherwise
name the bindings themselves as its credential. They are not secret, but a
variable a workspace can have sent anywhere should hold nothing but the one
value somebody meant to be sent somewhere.

Three alternatives were weighed, and the reason each lost is the argument for
this one.

**A table, read by the gateway.** The gateway has a database already, for the
breaker. But it is optional -- the gateway runs without one -- and a binding
that might not be readable is a check with a fallback somebody will one day
write as "allow". Worse, the table would be written through the API, and
anyone holding the API's database password could rebind ACME's key to their own
host. Today a compromised API can mint any turn token it likes, so it can
already make the gateway attach any credential *toward the hosts that
credential is bound to*; with the binding in the database, it could also choose
those hosts. Kept on the gateway, the binding holds against the API too: the
secret still only ever goes to Stripe. That is the property worth having -- the
destination of a secret is decided by whoever holds the secret, and by nobody
else.

**A naming convention carrying the workspace id.**
`OUTTURN_EGRESS_<workspace>_STRIPE_KEY` binds a workspace and cannot bind a
host, which is the half the client-credentials leak is about. It also makes a
key an organization shares across its workspaces -- the first arrangement in
[workspaces.md](workspaces.md) -- one copy of the secret per workspace, and
the copies are what drift.

**Committing bindings into the turn token**, the way `src/egress/commit.rs`
commits the rules. The commitment exists because the rules travel through the
runtime, which must not be believed about them. Bindings need not travel at
all: the one fact the gateway needs from the turn is which workspace it
belongs to, and `workspace_id` is already a signed claim on the token. A
commitment would add a scheme and make the API the authority on bindings,
which is the thing the table option loses for.

The cost of configuration is the one [egress.md](egress.md) accepted for the
internal-hosts list: changing a binding is a redeploy. Here that is barely a
cost, because adding a credential is already a manifest edit -- the binding is
written in the same change as the secret, by the same person, which is what
"one fact" means in practice.

## Who writes it

The operator, by editing the gateway's environment. Nothing in the API writes
a binding, so no authority is needed and none can be granted by mistake. If a
write surface is wanted later, it is a reserved platform authority --
`CredentialsBind`, listed in `workspace_assignable`'s exclusions beside
`WorkTake` and `GatewayFetch` -- and the gateway still holds the last word:
whatever the API stores is a request to the operator, not a binding.

## Where it is enforced

In the gateway, at the two places a variable is read: the static header in
`src/gateway/egress/mod.rs`, and the exchange in `src/gateway/egress/client.rs`.
The check sits beside `check_credential_variable` and runs before
`std::env::var`, so a variable the caller is not bound to is never read into
memory on its behalf.

It asks three things, from values the gateway holds itself:

- the workspace, from the validated turn token's `workspace_id` -- never from
  the request body or the rule;
- the host the request is actually going to, as `Shape` names it (no port),
  compared exactly against the binding's hosts. A rule may be
  `*.stripe.com`; a binding is a list of names, because a wildcard binding
  would bind the secret to hosts nobody has seen;
- for an exchange, the token URL the rule names, compared exactly against the
  binding's, both for the id variable and the secret variable. Both must bind
  it, so a pair cannot be assembled from halves bound to different places.

The check runs before the token cache is consulted as well as before an
exchange, so a held token is handed out only where the secret that bought it
may go. The cache itself is already keyed by workspace -- `slot` takes the
rule's leaf, which carries it, and `tokens_are_not_shared_between_workspaces`
holds it there -- so a token one workspace's exchange bought is never handed
to another's request even when their rules are alike.

The API reads the same variable, from the same ConfigMap -- the bindings are
not secret -- and uses it for two things only: refusing a rule at write time
whose variable is not bound to the writing workspace and host, with a message
saying so; and marking existing rules `unbound` when it lists them. Neither is
the enforcement. A row written by hand, or by an API that was wrong, is still
refused at the gateway.

## Existing rules

No migration. Rules keep their rows and their variable names; what changes is
that each variable now needs a binding before it attaches to anything. A
deployment that upgrades without writing bindings finds every credentialed
request refused, loudly, with the variable named -- which is the right failure
for a fix whose point is that the old behaviour was unsafe. A grace flag that
kept the old behaviour would be the leak with a setting.

To make the upgrade a reading exercise rather than archaeology, the gateway
logs at startup every variable under the prefix that has no binding, by name.
And the existing rules are the obvious first draft:

```sql
SELECT credential_env, workspace_id, host FROM egress_rules
WHERE credential_env IS NOT NULL ORDER BY credential_env;
```

A draft, to be read rather than pasted. Any row in it might be the theft this
closes, and a variable that turns up beside two workspaces is the one to look
at first.

## How it fits the three tiers

**Tier 1, the operator's APIs.** This is the tier it is for. The operator
holds the key, names the variable and binds it to the workspaces it serves and
the hosts it is for. A booking API every workspace uses is bound to `"*"`.

That is less generous than it reads. The hosts are what keep a secret from
leaving for somewhere the operator did not name, and `"*"` does not touch
them; the workspace list is a second limit, on who may use the credential at
its proper host. For a key that genuinely serves every workspace, `"*"` with a
fixed host list leaks nothing -- it says every workspace may call that API,
which is true. Listing ids instead would cost a gateway redeploy before each
new workspace could use a shared integration, and that friction is the kind
that gets worked around. `"*"` stands alone: beside ids it would be a list
claiming to be narrower than it is, and is refused.

**Tier 2, extensions.** A per-workspace credential from a consent flow cannot
be an environment variable, so it lives in the credential store integrations.md
describes. The binding moves with it: the store row carries its workspace and
the extension's approved hosts, fixed at authorisation, and the gateway asks
the same question of it that it asks of a variable. The check is one function
over two sources, not two checks.

**Tier 3, workspace-added hosts.** A workspace allowing a host of its own gets
a host and nothing else. It cannot attach an operator's credential there,
because no binding names that host; a workspace with its own key for its own
service needs the store, which is where a workspace-supplied secret belongs
anyway.

The per-agent narrowing in integrations.md composes with this rather than
replacing it: an agent's list narrows which rules it may use, and the binding
still decides where a rule's credential may go.

## Local development

Both local overlays declare `OUTTURN_CREDENTIAL_BINDINGS` empty, on the API
and the gateway, so there is somewhere to write it -- the same reason
`OUTTURN_INTERNAL_HOSTS` is declared. Empty binds nothing, so a rule carrying
a credential from before this existed is refused until its variable is bound
there; the API marks it `unbound` when listed.

## Not settled

- **Organizations.** Between one workspace and `"*"` there is nothing: a key
  an organization shares across its forty workspaces lists forty ids.
  Organizations, once they exist, are the unit for that -- bind to one and its
  workspaces follow.
- **Where the JSON lives at scale.** An environment variable is the smallest
  thing that works; a mounted file is what to reach for when it outgrows one.
  The check is the same either way.
