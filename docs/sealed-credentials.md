# Sealed credentials

A secret somebody types into the platform, stored where anyone can read it and
nobody but the gateway can use it -- and bound, inside the seal, to where it may
go. Designed, unbuilt.

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
  "workspace": "0192…-acme",
  "hosts": ["books.idataconnect.com"],
  "token_url": null,
  "header": "Authorization"
}
```

canonicalised (sorted keys, no whitespace) before it is fed to the AEAD.

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

## Who can seal, and why that is safe

Anybody can seal: the public key is public. That looks like a hole and is not.
To seal something is to choose a secret and a binding for it, and the person
doing so must already hold the secret. Somebody with write access to the
database can mint a credential of their own bound wherever they like -- and
attach their own key to their own requests, which they could do without the
platform. What they cannot do is rebind somebody else's, because that needs the
plaintext, and only the gateway can produce it.

The API still decides who may store a seal in a workspace -- `CredentialsWrite`,
assignable within a workspace -- and refuses a binding naming another workspace.
That check is for the people using the API honestly. The one a forged row
meets is at the gateway: the binding's workspace must equal the turn token's
`workspace_id`, the request's host must be in its hosts, and the rule's header
must equal its header. Those are the three questions credential-bindings.md
asks today, asked of the seal instead of the environment.

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
form on the web has. Where that matters, sealing is also a CLI command --
`outturn seal --workspace … --host …`, reading the key from stdin -- and the API
cannot tell the difference, since it only ever sees a ciphertext either way.

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

The gateway reads the row by id when a request needs it: one indexed read, cached
per replica by id and invalidated over LISTEN/NOTIFY on change, the way roles
are. The opened secret is cached the same way, in memory, never written
anywhere. A gateway with no database has no sealed credentials, and a request
needing one is refused with the same message as a credential that does not
exist.

## Revocation reaches a running turn

A removed host does not reach into a running turn, because the commitment is
copied on a token trade-in. A revoked credential must, for the reason
integrations.md gives about grants: somebody who revokes a key expects it to stop
being sent now, not after a long-horizon turn finishes. Here that falls out
rather than needing work, because the gateway reads the row by id on use rather
than carrying the secret in the token. Revoking sets `revoked_at`, wipes
`sealed` in the same statement, and notifies; the next request finds no
credential.

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
sealed once.

**The gateway's own provider keys are not part of this.** `GEMINI_API_KEY` and
the rest are not reachable from a rule, are not per workspace, and change when
the operator changes provider, which is a deploy anyway. They stay in the
environment.

**`OUTTURN_GRANT_KEY`** in integrations.md is a symmetric key for refresh tokens
the gateway writes itself. It could be this scheme instead -- the gateway seals
a refresh token to its own public key, with the grant's binding as associated
data -- and one scheme is better than two. A symmetric key works there only
because the writer and the reader are the same tier; a key a person types needs
a writer who cannot read it back, which is why this one is asymmetric.

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

## Not settled

- **Organizations.** A binding names one workspace or every workspace. A key an
  organization shares across its workspaces is the case between, and waits on
  organizations, as it does for environment bindings.
- **Who may read a credential's binding.** Showing an administrator where their
  key goes is the point; showing another member of the workspace which hosts a
  key reaches is probably fine and is not decided.
- **An HSM or KMS for the seal key.** The gateway holding `OUTTURN_SEAL_KEY` in
  its environment is the credential-holding tier being what it already is. A
  deployment that wants the private key in a KMS changes where the open
  happens, not anything above it.
