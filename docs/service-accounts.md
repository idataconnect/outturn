# Service accounts

How an agent comes to edit a file a workspace keeps in Google -- a
spreadsheet, a document, a shared drive -- without anybody's personal grant
and without the deployment registering anything with Google.

Designed, unbuilt. It rests on [sealed-credentials.md](sealed-credentials.md)
and [client-credentials.md](client-credentials.md), both built, and is the
first integration of the kind [integrations.md](integrations.md) calls tier 2
that needs no consent flow.

## The case

A person keeps the household's water bills in a spreadsheet they edit in the
browser. Each quarter they photograph the bill, work out what each tenant owes,
and add a row. They want an agent to do the arithmetic: read the figures from
the photo, show them and the calculation, and once they agree, put the row in.

A company wanting its agents to reach a shared drive is the same case at a
larger size, and the one this is for: files kept in Google because that is
where the people collaborate, reached by agents the way a bucket was.

## Why not OAuth

The three-legged grant integrations.md designs is the obvious answer, and the
wrong first one for a platform that is installed rather than hosted.

An OAuth client belongs to a Google Cloud project, and Google's review attaches
to that project. outturn has no project of its own -- every deployment would
register a client, and any deployment asking for Drive's broader scopes would
face the review itself, once each, for an install it did not write. The scopes
that need no review only reach files a person picks in Google's own picker, a
widget every deployment would have to configure too. And under all of it is
the grant store, the consent callback, revocation and the check for dead
grants: the most expensive thing integrations.md describes, built so that one
spreadsheet could be reached.

A **service account** needs none of it. It is an identity in the workspace's
own Google Cloud project, with a key the workspace downloads. People give it
access the way they give a colleague access: Share, paste its address, Editor.
Google reviews nothing, the deployment registers nothing, and what the agent
can reach is decided by the people who own the files, in the dialog they
already use.

The person's own grant is still worth having later -- edits under their own
name, or files in their own drive nobody has shared -- and is still
integrations.md's design. Nothing here is in its way: the gateway resolves a
credential to an access token, and a refresh token is another way of getting
one.

## One credential per workspace

integrations.md refuses the JWT-bearer grant for anything a tenant uses,
because there it meant one platform key that reached every tenant who had
consented. The grant is the same here and the shape is not. The key is the
workspace's, minted in the workspace's own project, and reaches only what was
shared with that workspace's account. A stolen key reaches one workspace's
shared files, which is the blast radius every other workspace credential has.

The rule that matters is the one integrations.md states, *one credential per
workspace, always*, and this keeps it. What it refuses is a key the operator
holds on everybody's behalf; nothing here lets an operator add one.

**Domain-wide delegation is never expressible.** Google lets a service
account act as any person in an organization, by naming them in the
assertion's `sub`. That is the widest reach Google offers, granted once by an
administrator and invisible to the people it acts as. The gateway never sends
`sub`, the binding has no field for one, and a delegated account used here is
an ordinary account that reaches what was shared with it.

## Two kinds of scope

The word means two things here, and keeping them apart is most of the design.

- **Which files** is decided in Google, by sharing. The account sees exactly
  what was shared with its address. outturn holds no list of files and has no
  way to widen it.
- **Which APIs** is the OAuth scope written into the assertion. `spreadsheets`
  reaches the Sheets API and nothing else: not Drive's listing, not Gmail, not
  a file's sharing settings.

The second matters more than it looks, because **the host does not bound a
Google token.** Most of Google's APIs answer under `googleapis.com`, and a
token with a broad scope works on all of them. An egress rule for
`sheets.googleapis.com` keeps the agent's requests there; it does not make the
token worthless elsewhere, and a token that leaks is good for whatever its
scope says. So the scope is bound like a host, and chosen as narrow as the
skill allows.

It also keeps a conduit out. Drive's `permissions` endpoint can share a file
with anybody or make it public, which turns a contained agent into a way to
publish what it reads -- the conduit integrations.md describes for
`gmail.send`. `spreadsheets` cannot reach it. A skill that needs Drive itself
asks for `drive.file` or `drive` and brings that question with it.

## The credential

A sealed credential of a new kind, `service_account`. The page takes the JSON
key file Google issues and splits it before sealing:

- **The plaintext** is the `private_key`, a PKCS#8 PEM. Nothing else in the
  file is secret.
- **The binding** gains two fields, both required for this kind and refused for
  every other:
  - `account` -- the `client_email`, which the assertion is issued as. It is
    what the page shows, because it is the address a person shares files with.
  - `scope` -- the scope the gateway asks for, one space-separated string sent
    as written, as client-credentials rules treat theirs.

  `token_url` is the file's `token_uri` and required, as for a client pair.
  `header` is required and must be `Authorization`. `hosts` are the API hosts
  the token may be attached on.

```json
{
  "credential": "0192…",
  "kind": "service_account",
  "workspaces": ["0192…-household"],
  "hosts": ["sheets.googleapis.com"],
  "header": "Authorization",
  "token_url": "https://oauth2.googleapis.com/token",
  "account": "water-bills@example-project.iam.gserviceaccount.com",
  "scope": "https://www.googleapis.com/auth/spreadsheets"
}
```

The scope is the binding's, not the rule's, which is where this differs from a
client pair. A client secret was issued for one API, and the scope on its rule
narrows within what the provider already limits. A service account key mints
tokens for any Google API its project has enabled, so the scope decides what
the key is, and it has to be as fixed as where it may go. Someone who can write
rules, or a runtime that cannot be trusted, would otherwise turn a key that was
shared for one spreadsheet into a token for everything the account can see.

Because all of it sits in the sealed binding, **the rule needs nothing new.**
It names a credential and a header, as any sealed rule does, and the sealed
leaf already covers both; scope, account and token URL are covered by the
AEAD tag, and a binding edited after sealing does not open. No change to
`egress_rules`, to the commitment, or to what the runtime carries.

`deny_unknown_fields` on the binding means a gateway built before this refuses
a `service_account` seal rather than misreading it. API and gateway deploy
together, the runtime need not.

## The exchange

RFC 7523, as Google speaks it. When a request needs a token and the cache holds
none fresh enough, the gateway:

1. Builds the assertion: header `{"alg":"RS256","typ":"JWT"}`, claims `iss` the
   bound account, `aud` the bound token URL, `scope` the bound scope, `iat` now
   and `exp` an hour on, the longest Google accepts.
2. Signs it with RSA PKCS#1 v1.5 over SHA-256, through `aws-lc-rs`, which the
   gateway already compiles for TLS. The key is parsed when the seal is opened
   and cached parsed beside it; a seal whose plaintext is not an RSA PKCS#8 key
   is refused then, with a message, rather than on every request. `aws-lc-rs`
   rather than a pure-Rust RSA crate because its private-key operations are
   constant-time and it adds nothing to the build.
3. Posts `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer` and the
   assertion to the token URL.

Everything after the request is client-credentials.md's, unchanged: the same
vetted transport with no redirects, the same size limit and timeout, the token
cached in memory per replica, refreshed on demand with single-flight, evicted
on a 401 and on revoking the credential, never written anywhere. The same table
of what a failed exchange tells the agent, where `invalid_grant` -- Google's
answer to a key that was deleted in its console, or a clock badly wrong --
reads as the configuration problem it is.

The cache is keyed by the credential and its generation, so rotating a key
starts afresh.

## The skill

A Sheets skill, generic: nothing in it names a spreadsheet. It documents
reading a range (`values.get`), appending rows (`values.append`) and updating
cells (`values.update`), with `valueInputOption=USER_ENTERED` so a formula is a
formula and a date is a date; declares `sheets.googleapis.com`; and says which
scope it needs, so a credential bound to less shows as unmet the way a missing
host does.

The writes carry approval frontmatter, so the agent reading figures off a photo
cannot put them in the sheet until a person has seen them -- enforced by the
gateway, rather than resting on the agent remembering to ask. The water-bill
case's "confirm with me first" is then an approval rule
([approval-rules.md](approval-rules.md)) rather than a sentence in a prompt,
and the quarter's row is one approval in the queue.

The spreadsheet's id belongs to the agent, not the skill: in its instructions,
or a note in its `agent/` scope. The Sheets scope cannot list files, which is
also why the agent cannot go looking for others.

## Who can do what

The people in a company who share an agent see this through the roles they
already have:

- **Configuring it** -- storing the key, writing the rule -- is
  `credentials:write` and `settings:update`, as for any sealed credential.
  Using the agent never lets anybody change where the key goes or what it
  mints.
- **Using it** is using the agent: whoever may start a conversation with it can
  have it edit what was shared with its account, and whoever may read its
  sessions can see what it read and wrote. Per-agent narrowing
  ([authorities.md](authorities.md#narrowing-an-authority-to-some-agents))
  applies as it does to everything else.
- **Which agents** is every agent in the workspace, today, because egress rules
  have no agent column. A support agent and a billing agent in one workspace
  both reach the billing sheet. Closing that is per-agent egress in
  integrations.md, which may only narrow; until it exists, an agent that must
  not reach a sheet belongs in another workspace.
- **Whose name is on an edit** is the service account's, in the file's version
  history. The transcript says which conversation, and therefore which person,
  asked for it.

## Setting it up

The part most likely to fail is not the code. A person who has never opened a
cloud console has to:

1. Create a Google Cloud project. Free, and possible from a personal account.
2. Enable the API the skill uses -- the Sheets API here.
3. Create a service account, and a JSON key for it.
4. In outturn, connect the key on the skill's Connections, which shows the
   account's address.
5. Share the file with that address, as Editor.

Three traps, each worth a line in the guide where somebody meets it:

- **Key creation may be switched off.** Organizations created in recent years
  have a policy forbidding service account keys by default. A personal project
  has no organization and is unaffected; in a company an administrator has to
  relax it for the project.
- **Sharing outside the domain may be refused.** The account's address is not
  in the company's domain, so an organization limiting sharing to its own
  domains, or a shared drive limited to members of it, refuses to add it until
  an administrator allows it.
- **The account has no storage of its own.** It cannot create a file in a drive
  of its own; anything it creates has to be in a shared drive. Editing a file
  somebody shared with it is unaffected.

Dogfooding is how we find out whether that list is short enough. If people give
up at step 1, that is a finding about this design, not about them.

## What this does not settle

- **Acting as a person.** Edits under somebody's own name, and files nobody
  shared, are integrations.md's authorization-code grant. Unbuilt; nothing here
  is in its way.
- **Drive and Docs.** Another skill and another scope each. Docs is harder for
  an agent than Sheets -- its edits are positional -- and Drive's broad scopes
  bring the sharing conduit back, so each is its own decision.
- **Typed tools.** A Sheets tool would shape the arguments a weaker model gets
  wrong. integrations.md records that as a fork to take when an integration
  demands it; this one may.
- **Other providers.** RFC 7523 is not Google's alone, and the exchange is
  written to the RFC. The binding's `account` and `scope` are what any of them
  would need; a provider wanting more fields gets them when it arrives.

## Order of work

1. Core: the `service_account` kind and the binding's `account` and `scope`,
   parsed and refused as strictly as the rest; tests that a seal of this kind
   cannot be used as a static header or a client pair, and the reverse.
2. Gateway: the assertion, the signing and the exchange, through the existing
   transport and token cache. Tests against the fake gateway's token endpoint,
   which checks the signature with a test key, and for each binding refusal:
   wrong workspace, host or header, and a binding whose scope was edited after
   sealing.
3. API and page: storing the kind, the key file split in the browser, the
   account's address shown on the connection.
4. The Sheets skill, with approval frontmatter on its writes.
5. The setup guide, then dogfooding on the water-bill spreadsheet.
