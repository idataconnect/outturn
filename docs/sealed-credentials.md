# Sealed credentials

A secret somebody types into the platform, stored where anyone can read it and
nobody but the gateway can use it -- and bound, inside the seal, to where it may
go. Built for static header credentials: `src/egress/seal.rs`, the
`credentials` table, the gateway's read in `src/gateway/egress/sealed.rs`, the
API in `src/api/credentials.rs`, sealing in the browser (`ui/src/lib/seal.ts`)
and with `outturn-seal`. Client-credentials pairs are not yet; see *Order of
work*.

It replaces the environment as the place an egress credential lives. Today a
credential is an `OUTTURN_EGRESS_` variable on the gateway, bound by
`OUTTURN_CREDENTIAL_BINDINGS` beside it ([credential-bindings.md](credential-bindings.md)),
and adding one is a manifest edit and a pod roll. That was the right trade while
the operator was the only person who ever held a key. It is the wrong one for
the deployment this platform is for, where an operator publishes an
integration and each of their clients brings their own key to it.

## What has to survive the move

credential-bindings.md weighed a table and rejected it, and the reason it gave
is the specification for this design:

> the table would be written through the API, and anyone holding the API's
> database password could rebind ACME's key to their own host ... the
> destination of a secret is decided by whoever holds the secret, and by nobody
> else.

So the property is not "the secret is encrypted at rest". It is that **nobody
who can write the database can change where a secret goes**. Encryption alone
does not give that: a row holding a ciphertext and a separate column saying
which hosts it may reach lets the database's writer edit the column and leave
the ciphertext alone. The binding has to be part of what was sealed.

It is worth being exact about what that does not include, because the first
draft of this document claimed more. The seal fixes where *a given secret* may
go. It does not fix *which secret* a credential holds: anyone can seal, so a
database writer can put their own key under somebody else's credential id, bound
exactly as the original was, and the victim's agent then works in the attacker's
account -- charges land in the attacker's Stripe, uploads in the attacker's
Drive. That is destination without provenance, and it is the same power such a
writer already has over the rules and skills that steer an agent, which are
unsigned rows too. Authenticating the credential alone would buy little while
those stay forgeable. What this design does instead is make a swap visible: see
*Who can seal* below.

It also gave a second reason, that the gateway's database is optional and "a
binding that might not be readable is a check with a fallback somebody will one
day write as allow". That one does not carry over, for a reason worth being
precise about: what the gateway reads here is the secret itself, not a
permission about it. A credential the gateway cannot read is a credential it
does not have, and a request needing one is refused because there is nothing to
attach. There is no allow branch to fall back to.

## The seal

The gateway holds an X25519 key pair: `OUTTURN_SEAL_KEY`, gateway only, the way
`OUTTURN_TOKEN_SECRET` is API only. Its public half is `OUTTURN_SEAL_PUBLIC_KEY`
on the API, which hands it to whoever is about to seal something.

A credential is sealed with HPKE (RFC 9180, base mode, X25519 / HKDF-SHA256 /
AES-256-GCM) to that public key. The plaintext is the secret and nothing else.
The **binding is the associated data**:

```json
{
  "credential": "0192…",
  "kind": "static",
  "workspaces": ["0192…-acme"],
  "hosts": ["books.idataconnect.com"],
  "token_url": null,
  "header": "Authorization"
}
```

stored as the exact bytes that were fed to the AEAD, beside a parsed copy for
reading. Bytes, because re-canonicalising JSON to check a tag is a way for a
serializer upgrade to make every valid seal fail to open.

- **`kind`** is `static`, `client_id` or `client_secret`. The gateway attaches
  only a `static` credential as a header, and exchanges only a `client_*` pair.
  Without it a client secret, bound to the resource host for the token it buys,
  could be named by a rule as a plain header and sent there raw -- which the
  environment path did until it was fixed, by refusing a variable bound to a
  token URL as a header credential.
- **`workspaces`** is a list of ids, or the sole value `"*"`, read by the same
  rule as `OUTTURN_CREDENTIAL_BINDINGS`: `"*"` alone or not at all.
- **`hosts`** are exact names, as for environment bindings: no wildcard, no
  port, no scheme, refused at write time by the API with a message and refused
  again by the gateway, which parses the opened binding as strictly as it parses
  the environment.

The HPKE `info` is a fixed label, `outturn seal v1 egress-credential`, so a
ciphertext made for anything else -- a manage token, below -- can never be opened
as a credential, or the reverse.

Associated data rather than plaintext, because it then serves both readers. The
API and the browser can read the binding without decrypting anything -- to show
an administrator where their key goes, to refuse at write time a rule that names
a host the credential is not bound to, to mark existing rules that could never
attach. And the gateway cannot be lied to about it: the stored row carries the
binding in the clear, the gateway feeds that row's binding back in as
associated data when it opens the seal, and a binding edited after sealing fails
the tag. The ciphertext does not open, so there is nothing to attach.

The credential's own id is in the binding, so a ciphertext copied onto another
row -- another workspace's, or a rule that names a different credential -- fails
the same way. That is the shape integrations.md already chose for refresh
tokens, with the grant's id, workspace and owner as associated data, applied to
a key somebody typed.

`header` is bound too. Without it, a rule could attach a credential bound to the
right host in a header the host logs -- a query string's worth of exposure
reached by renaming one field.

## Who can seal, and what that allows

Anybody can seal: the public key is public. To seal something is to choose a
secret and a binding for it, and the person doing so must already hold the
secret. So somebody with write access to the database cannot rebind a key they
do not have -- that needs the plaintext, and only the gateway can produce it.

What they can do is seal a key they *do* have under another workspace's binding,
and swap it in. That is the provenance gap described above, and it is closed by
making it visible rather than impossible:

- **A fingerprint, computed by the gateway.** When a credential is stored, and
  on request after, the gateway opens it and returns an HMAC of the secret under
  a gateway-only key, shortened for display. The page shows it beside the
  credential, as `…k3f9`, and the owner who sealed it saw the same value then. A
  key swapped underneath them shows a different one. Nobody else can compute it,
  so a forger cannot make theirs match.
- **Every store and rotation is recorded** in the credential's history with who
  did it, through the API. A row changed without such a record is one the
  database's writer changed, and the fingerprint is what says so.

The API still decides who may store a seal in a workspace -- `CredentialsWrite`,
assignable within a workspace -- and refuses a binding naming any workspace but
the writer's own, `"*"` included. That check is for the people using the API
honestly. The one a forged row meets is at the gateway: the turn token's
`workspace_id` must be among the binding's workspaces, the request's host must
be in its hosts, the rule's header must equal its header, and its kind must be
the one the rule uses it as. The first three are the questions
credential-bindings.md asks today, asked of the seal instead of the environment.

## Where it is sealed

**In the browser.** The page fetches the public key, seals with WebCrypto (an
X25519 and HKDF implementation small enough to vendor, or `hpke-js`), and sends
the ciphertext. The API never holds the plaintext, not even for the length of a
request, so neither does its logging, its error reporting or a core dump of it.

Worth being honest about what that buys. It protects against everyone who can
read or write the API's database and against an API that is later compromised.
It does not protect against an API that is compromised *while* someone types
their key into a page it serves, because that API serves the script doing the
sealing. Nothing a web page does can; it is the same exposure every credential
form on the web has.

Where that matters, sealing is also a CLI command -- `outturn seal --workspace …
--host …`, reading the key from stdin -- and the API cannot tell the difference,
since it only ever sees a ciphertext either way. **But only with the public key
pinned.** A CLI that asked the API for the key would seal to whatever a
compromised API served, which could be the API's own. So the CLI seals only to a
key it already holds -- its fingerprint and id distributed with the operator's
manifests, as the gateway's own key is -- and refuses one the API serves that
does not match. A rotation of the seal key means distributing a new pin.

## Where it is stored, and how the gateway gets it

```
credentials (
  id uuid primary key,          -- UUIDv7
  workspace_id uuid not null,
  name text not null,           -- what an administrator calls it
  binding jsonb not null,       -- the associated data, as sealed
  sealed bytea not null,        -- HPKE enc || ciphertext
  key_id text not null,         -- which seal key
  created_by uuid,
  created_at timestamptz,
  revoked_at timestamptz
)
```

Returned to a browser without `sealed`, though returning it would leak nothing.

An egress rule stops naming a variable and names a credential:
`credential_id`, beside the existing `credential_env` for as long as the
environment path lives. The commitment's leaf for such a rule carries the
credential id, so a runtime cannot swap one credential for another without
failing the proof -- the same reason client-credentials rules got a leaf tag of
their own.

The gateway reads the row by id when a request needs it: one indexed read,
cached per replica, with the opened secret cached beside it in memory and never
written anywhere. A gateway with no database has no sealed credentials, and a
request needing one is refused with the same message as a credential that does
not exist.

The cache is specified here rather than borrowed, because the obvious pattern
loses a revocation. Reading a row, then inserting what was read, races an
invalidation that lands in between: the stale entry goes in after the eviction,
and a revoked row never changes again, so nothing ever evicts it. So:

- **Each id has a generation**, bumped by every invalidation. A load records the
  generation before it reads and inserts only if it is unchanged.
- **Entries expire** after a minute whatever happens, so a missed notification
  costs at most that.
- **A dropped listener clears everything**, and nothing cached is used until it
  is back -- a gateway that cannot hear about revocations must not go on
  trusting what it holds.
- **Invalidating a credential evicts the access tokens it bought**, for a
  client-credentials pair, from the token cache as well.

The role cache in the API (`api::role::postgres`) follows the first three: a
per-workspace generation, a minute's expiry, and a listener that clears and
bypasses the cache from the moment it is seen to drop until it is listening
again.

## Revocation reaches a running turn

A removed host does not reach into a running turn, because the commitment is
copied on a token trade-in. A revoked credential must, for the reason
integrations.md gives about grants: somebody who revokes a key expects it to stop
being sent now, not after a long-horizon turn finishes. Here that falls out
rather than needing work, because the gateway reads the row by id on use rather
than carrying the secret in the token. Revoking sets `revoked_at`, wipes
`sealed` in the same statement, and notifies; once the notification arrives --
a matter of milliseconds, not the same breath, since the gateway is another
process -- the next request finds no credential. A notification lost on the way
costs at most the cache's minute.

That stops honest use. It does not bind anybody who can write the database,
because an old ciphertext still opens -- restored from a backup, it is a working
credential again. So revoking a key that was compromised or misused means
revoking it at the provider too, and the page says so when somebody revokes.
Only rotating the seal key invalidates every copy a backup holds.

## Rotation

Of the secret: seal a new one and point the rule at it, or update the row in
place (same id, new seal). In place is the common case -- the key a client
rotated at their provider is the same credential to them -- and the
notification drops the cached plaintext.

Of the seal key: the gateway accepts two private keys, each with an id, the
shape `OUTTURN_TOKEN_PUBLIC_KEY` already takes during a token key rotation.
Resealing under the new key needs the plaintext, so it is the gateway's job:
an operator command that opens each row under the old key and seals it under
the new one, after which the old key is removed. Nobody else is ever in a
position to do it, which is the point.

## What this replaces

**`OUTTURN_EGRESS_` variables and `OUTTURN_CREDENTIAL_BINDINGS`.** A sealed
credential is the same fact -- a secret, its workspaces, its hosts, its token
URL -- with the binding sealed rather than configured, and a pod roll replaced
by a row. The environment path stays while rules that use it exist, and new
rules are written against sealed credentials. Once none remain, the prefix rule
and the bindings variable go, and with them the last credential that is a
manifest edit to add.

A key the operator shares with every workspace -- the booking API in
credential-bindings.md -- is a seal whose binding says `"workspaces": "*"`, held
in the platform workspace. Readable by the gateway for any workspace's turn,
sealed once. Only the operator may store or rotate one, through the platform
routes; every workspace route refuses `"*"`.

**The gateway's own provider keys are not part of this.** `GEMINI_API_KEY` and
the rest are not reachable from a rule, are not per workspace, and change when
the operator changes provider, which is a deploy anyway. They stay in the
environment.

**Not `OUTTURN_GRANT_KEY`.** integrations.md keeps refresh tokens under a
symmetric key only the gateway holds, and that is right, for a reason this
document's first draft missed when it proposed one scheme for both. A ciphertext
under a key only the gateway holds is one only the gateway could have made, so
it proves the gateway wrote it. A seal to a public key proves nothing about its
author. For refresh tokens that difference is the whole defense: under a public
key, somebody who completed a consent flow for their *own* Google account could
seal the resulting token under a victim's grant and have the victim's agent
working in their mailbox. The two schemes answer different questions -- who may
read, and who wrote -- and both are needed.

## The credential is a rule's, not a skill's

A skill declares the hosts it reaches and the header its API expects; it never
names a credential. An integration is a skill, a rule permitting its host, and a
sealed credential bound to that host -- and the three meet in the rule. That is
what lets an operator publish one skill to every client and each client seal
their own key against it, with nothing about any client's key in the skill.

The OpenAPI wizard's last step becomes a field rather than an instruction. Today
it ends by telling the operator to set an environment variable on the gateway,
which a browser cannot do; with this, it ends by asking for the key, sealing it
in the page, and writing the rule. See *Hosts and credentials* in
[openapi-wizard.md](openapi-wizard.md).

## Order of work

Three phases, the first of which makes an integration with a key work end to
end without touching a deployment.

**Phase 1 -- the gateway uses a sealed credential.** Built.

1. `src/egress/seal.rs`: the binding type, its exact bytes, and opening under
   the fixed label, with HPKE from the `hpke` crate (X25519, HKDF-SHA256,
   AES-256-GCM). Tests for a tampered binding, a seal moved onto another id, a
   wrong kind and a wrong key.
2. Keys: `scripts/dev-secrets.sh` generates `OUTTURN_SEAL_KEY` for the gateway
   and `OUTTURN_SEAL_PUBLIC_KEY` for the API, as it does the token keys. One
   environment change per deployment, once, rather than one per credential.
3. A migration: the `credentials` table, with a generation per row, and
   `egress_rules.credential_id`. A rule names a variable or a credential,
   never both.
4. The commitment: a leaf tag of its own for a rule naming a credential,
   covering the id and the header, so a runtime cannot swap one credential for
   another. Absent, a rule hashes exactly as it did.
5. The gateway: the row read by id, opened, checked against the token's
   workspace, the request's host, the rule's header and the use's kind, and
   cached by the rules above. No database, no sealed credentials.
6. The API: create, list without the ciphertext, rotate and revoke, under a new
   pair of authorities, `credentials:write` and `credentials:read`; and a rule
   naming a credential refused at write time when the binding does not cover
   its host and header.
7. `outturn-seal`, a small binary that reads a secret from stdin and seals it
   to a pinned public key: the CLI path above, and how a key is sealed until
   the page exists.

A rule gains a field that travels from the API through the runtime to the
gateway, so the three tiers deploy together, as they did for client
credentials: an older runtime drops the field and the request is refused.

**Phase 2 -- the page.** Built, but for the last item: sealing in the browser
(`@hpke/core`, with a seal it made pinned and opened in the Rust tests);
Connections on a skill's page and in Settings, each host's key connected,
tested with one GET, replaced and revoked; the fingerprint, which the gateway
returns with a test call rather than behind an endpoint of its own; and the
OpenAPI wizard ending on connecting a key rather than a gateway variable. Not
yet: client-credentials pairs as sealed credentials of kind `client_id` and
`client_secret`, which still name environment variables.

**Phase 3 -- retire the environment path.** Once no rule names an
`OUTTURN_EGRESS_` variable, the prefix rule and `OUTTURN_CREDENTIAL_BINDINGS`
go.

## Not settled

- **Organizations.** A binding names one workspace or every workspace. A key an
  organization shares across its workspaces is the case between, and waits on
  organizations, as it does for environment bindings.
- **Who may read a credential's binding.** Showing an administrator where their
  key goes is the point; showing another member of the workspace which hosts a
  key reaches is probably fine and is not decided.
- **Provenance for rules and skills.** The fingerprint makes a swapped
  credential visible; nothing yet makes a swapped rule or skill version visible
  in the same way. If credentials ever get a gateway-signed provenance, they are
  the wrong place to start.
- **An HSM or KMS for the seal key.** The gateway holding `OUTTURN_SEAL_KEY` in
  its environment is the credential-holding tier being what it already is. A
  deployment that wants the private key in a KMS changes where the open
  happens, not anything above it.
