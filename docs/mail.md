# Mail

Designed, not built. What follows is the plan for outturn sending email, and
the order it is built in. Receiving mail is a different piece of work, IMAP
under [triggers.md](triggers.md); the two share a page and nothing else.

Three things want mail, and none of them can be done well without it:

1. **Approval notifications.** A held request waits on a page until somebody
   happens to look. One that moves money should reach the people who can
   answer it.
2. **Signing in by email**, chosen at the platform level as password only,
   email only, or either (roadmap, *Signing in by email*).
3. **Invitations and password resets.** Today an admin chooses a person's
   first password, and nobody can reset a forgotten one.

So mail is built once, as a platform capability, and those three use it.

## Shape

```
  API (anything that wants a message sent)
    │  writes a mail_messages row and a mail.send job, in one transaction
    ▼
  jobs table ── claimed by the API worker, retried with backoff
    │  renders the message, mints a token, asks the gateway to send it
    ▼
  gateway ── opens the sealed relay credential, vets the relay's address,
    │         speaks the protocol
    ▼
  relay (SMTP: a provider, a mail server, a local catcher)
```

### The queue is the one outturn has

The `jobs` table is already a distributed queue: leases, a reaper for work
whose claimer died, retries, and a worker in every API replica. A message is
one more job kind, `mail.send`, and gets all of that for nothing -- it
survives a restart, it is sent by exactly one replica, and a relay that is
down for an hour delays mail by an hour rather than losing it. A separate
queue would be a second thing to run, monitor and back up, for a load that is
a few messages a minute.

A row in `mail_messages` is written beside the job, in the same transaction:
recipient, what it is for (`approval`, `sign_in`, `invitation`, `test`),
state (`queued`, `sent`, `failed`, later `bounced`), attempts, the last error,
and the provider's message id once sent. It is the answer to "why did I not
get the email", which otherwise has none. The job carries the row's id and
nothing else, so a retry sends what was written rather than something
re-rendered from state that has moved.

Retries back off -- a minute, five, thirty, two hours -- and stop after a day.
A message whose purpose has lapsed by then (a sign-in link that would have
expired, an approval already answered) is marked failed rather than sent late:
the worker checks before each attempt.

### The gateway sends it

The rule in this repository is that the gateway holds the credentials and
nothing else does. A relay's password is a credential, so the API never holds
it: the worker asks the gateway to send, as it asks the gateway to fetch a
document (`crates/api/src/fetch.rs`), with a token it mints
for the request -- the `mail_send` role, a minute to live, committing to the
one relay it names. The gateway:

- opens the sealed credential, which only it can ([sealed-credentials.md](sealed-credentials.md));
- resolves the relay and refuses a private address unless the operator named
  it in `OUTTURN_INTERNAL_HOSTS`, as for any other host -- a relay inside the
  cluster is an operator's decision, said where every such decision is said;
- refuses plain SMTP without TLS to anything not so named, since the
  credential travels in it;
- sends, and returns the provider's message id or the relay's error, which the
  worker writes to the row.

An agent cannot send mail through this: the role is minted by the API for the
API's own messages, and no turn token carries it. Agents sending mail on a
workspace's behalf -- a reply to a supplier -- is a different feature, an
egress rule to a mail API, and goes through the gate like any other write.

### Protocols, not vendors

As with models, the gateway speaks protocols. The first is **SMTP**, with
submission on 587 (STARTTLS) or 465 (implicit TLS). Every sending provider
accepts it, as does a company's own mail server and the catcher a developer
runs locally. A provider is then configuration: Postmark is `smtp.postmarkapp.com`, port 587,
with the server token as both username and password, and is what the
settings page suggests by default.

A provider's own HTTP API earns a file of its own when it gives something
SMTP cannot. Postmark's does: a message stream per kind of mail, a message id
returned rather than parsed out of a reply line, and bounce and delivery
webhooks. Bounces matter once signing in depends on mail -- a link sent to an
address that no longer exists should show as bounced beside the person, not as
silence -- so that is when it is added, as a second protocol beside SMTP, with
its webhook moving the `mail_messages` row to `bounced`.

## Configuration

Platform level, on **Platform → Mail**, beside Models: the sender's address
and name, the relay's host, port and TLS mode, the username, and the password
sealed in the browser to the gateway's key like a provider key. Stored in the
database, not the environment, for the same reason models are: it is chosen,
tested and changed by an operator signed in, not by whoever last edited a
manifest.

**Nothing relies on mail until a test message has arrived.** The page sends one
to the operator's own address and asks them to confirm it came; until then
mail is *configured* but not *working*, and the features that need it say so
rather than queueing messages nobody will receive. Changing the relay or its
credential sets it back to untested. This is what makes email sign-in safe to
offer: a deployment that switched to email-only with mail that does not
arrive has locked everybody out, the operator included -- so that switch is
refused while mail is untested, and `api bootstrap` stays as the way back in.

A workspace sending from its own domain -- its agents' notifications from
`agents@acme.example.com` rather than the platform's address -- is a later
override at the workspace level, using the same table. Not first.

## What sends

Each a template, rendered by the API: plain text and a simple HTML part, the
brand name from `OUTTURN_BRAND_NAME`, and nothing a person did not need --
no tracking pixels, no remote images.

- **A held request**, to the people who may answer it -- the roles chosen for
  high-risk requests where it is high risk, everybody who answers approvals
  otherwise ([auto-approval.md](auto-approval.md)). It says what is asked, in
  the words the approval page uses, and links to it; it does not let a reply
  to the email approve anything. One message per request at first, and a
  person may turn them off for themselves; a digest if one per request turns
  out to be too many.
- **The test message**, from the Mail page.
- **Sign-in links, invitations and resets**, with signing in by email. A link
  is single-use, short-lived (fifteen minutes), stored only as a hash, and
  bound to the browser that asked for it where that does not make a phone that
  opens mail in another app useless -- designed with that feature, not here.

## Order

1. **Outgoing mail.** The settings and their page, the sealed credential, the
   `mail_messages` table and `mail.send` job, SMTP in the gateway, and the
   test message.
2. **Approval notifications**, and a person's own switch for them.
3. **Signing in by email, invitations and resets.**
4. **A provider's HTTP API and bounces**, when bounces start to matter.

## Testing

The integration suite gets a fake SMTP server beside `fake_gateway.rs`, which
records what it was sent and can refuse, hang or drop -- so retries, backoff
and the lapsed-purpose check are tested for behavior, not timing. Local
development deploys a mail catcher with `--with mail`, so a developer sees
every message the platform sends at a local address and none of them leave
the machine.
