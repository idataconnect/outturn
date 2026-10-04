# The admin agent

An agent a workspace administrator talks to about the workspace itself: its
skills, its policies, its usage. Designed, unbuilt.

The platform's configuration is getting precise in ways most administrators
will not want to write by hand. An [auto-approval](auto-approval.md) policy is a
selector and a conjunction of field tests; an operation's annotations are keyed
by `operationId`; a credential's binding is a list of hosts. Each is simple, and
together they are a small language somebody has to learn. "Bleargh Bot doesn't
need to ask before moving messages to spam" is how an administrator thinks about
it, and an agent that turns that into the row -- shows it back, shows what it
would have done, and waits -- is how most people will want to configure this.

The hazard is obvious and is the whole of this document: an agent that can
change what other agents may do is the most valuable thing on the platform to
inject an instruction into.

## The rule

**The agent proposes; a person confirms; the confirmation is out of the agent's
reach.** Not out of its instructions -- out of its reach. Everything below is
arranged so that a completely compromised admin agent, one whose every token is
in an attacker's hands, can read what its administrator can read and propose
changes, and cannot make one.

## A token of its own, which the runtime never holds

A management turn is a turn like any other: queued, claimed by a runtime, run in
the sandbox, with a turn token for the gateway. What it adds is a second
credential, for the API, minted when the turn is prepared:

- **Audience `outturn:manage`.** A third audience beside `outturn:api` and
  `outturn:gateway`, and each validator insists on its own, as today. The API's
  ordinary routes refuse it; the manage routes refuse everything else.
- **Subject: the administrator** whose session this is. Never a trigger turn --
  a turn with no `user_id` gets no manage token, so nobody's schedule can
  administer a workspace.
- **Authorities: theirs, narrowed.** The intersection of the person's resolved
  authorities with a fixed set that management may use, re-resolved on every
  request -- membership included, not only the roles' contents, so somebody
  removed from the workspace mid-session loses it on the next call rather than
  when the token lapses.
- **Minted only for the session's owner.** Only when the turn's `user_id` is the
  session's own, with that id as the subject. A turn somebody else started in
  the session -- which the session rules below also forbid -- gets none.
- **Bound to the session, and to a live turn.** Every `/v1/manage/` request
  checks that the session's turn is currently leased, so a token that outlives
  its turn is refused. On a trade-in the API **re-mints** it and recomputes the
  management turn's commitment itself, from the session rather than from
  anything the runtime presents; trade-ins elsewhere copy commitments forward,
  and copying a manage token forward is how it would outlive its turn.

The runtime must not hold it, because the runtime is what is assumed
compromised, and a manage token in a runtime's hands is an administrator's
access to anyone who has taken the pod. So the API **seals** it to the gateway
([sealed-credentials.md](sealed-credentials.md)) -- the binding naming the
session, the workspace and the manage prefix, under an HPKE label of its own so
it can never be opened as an egress credential or the reverse -- and the sealed
blob travels in the turn's commitment as a rule's credential would. The runtime carries
something it cannot open, cannot rebind, and cannot use anywhere but through
the gateway, toward one prefix, for one session.

## The path: through the gateway, to one prefix

The admin agent calls the API the way every agent calls everything:
`fetch_url`, through the gateway. The egress guard refuses `outturn-api` to
every turn, deliberately, and still does -- this is not an exception to it but a
rule kind of its own, committed only into a management turn's token:

- the rule names a fixed host, `outturn-manage`, which no DNS answers and no
  workspace can write a rule for;
- the gateway maps it to the API's internal address, which it already knows;
- it forwards only paths under `/v1/manage/`, and only the methods the prefix
  serves;
- it attaches the opened manage token and nothing else. A request's own headers
  are refused if they carry one, as for every credential.

The API, for its part, mounts every management endpoint under `/v1/manage/` and
accepts an `outturn:manage` token nowhere else. Either check alone would hold;
both are there so that a mistake in one -- a route mounted in the wrong place, a
prefix compared without its trailing slash -- is caught by the other.

A management turn carries no other egress rule. An agent that can read the
workspace's usage, transcripts and configuration and also reach an arbitrary
host is an exfiltration path with a chat window, so the management rule is the
only one in its commitment, whatever the workspace allows.

The gateway is not the only way out, and the others have to be closed too:

- **The reader's browser.** A reply is markdown, and an image in it is fetched
  the moment it renders. Closed for every agent, not only this one: images in
  replies render as links, and the page's Content-Security-Policy refuses images,
  media and frames from elsewhere.
- **Storage.** A management turn gets no agent or workspace storage scope --
  session scope only, readable by its owner -- so it cannot leave what it read
  where another agent, with egress, will find it.
- **Its own configuration.** The admin agent's prompt, skills, route and settings
  are the operator's and are not writable through any workspace route, so
  nobody makes it a different agent by editing it.
- **Co-residency.** A runtime pod taken over through a sandbox escape reads what
  its turns read, which for a management turn is an administrator's view of the
  workspace. That is the boundary AGENTS.md accepts, at a higher price; the
  follow-up is scheduling management turns on a pool of their own, never beside
  turns that have egress. Its own skills come
from the operator, documenting the manage API -- generated by the
[OpenAPI wizard](openapi-wizard.md) from the API's own specification, which is
the wizard's first customer being the platform.

## Writes are proposals

Everything under `/v1/manage/` that would change something creates a
**proposal** instead:

```
manage_proposals (id, workspace_id, session_id, proposed_by_job,
                  kind, payload jsonb, preview jsonb,
                  state, decided_by, decided_at, expires_at)
```

`preview` is what the proposal would do, worked out by the API when it is
made: for a policy, the history replay from auto-approval.md -- these forty-one
would have gone through, these nine would still ask; for a skill change, the
version diff; for a credential binding, the hosts. The agent reads it back to
the administrator in words; what the administrator *decides on* is the card,
and the card is not the agent's:

- **It renders from the API only.** The proposal's id arrives in an event the
  API emits into the transcript, as `chat.held` does, never from anything the
  guest wrote; the card fetches `GET /v1/proposals/{id}` with the browser's own
  token and renders it as plain text. An agent describing a harmless change in
  its reply beside a card showing a different one is the attack, and the card is
  the one that is true.
- **A skill change leads with what it does to safety.** Above the diff, derived
  by the API: gates added, removed or loosened (`requires`, `matches`, `covers`,
  `binds`, `risk`, `auto`), hosts and headers declared or dropped. A change that
  loosens a gate is proposed on its own, not inside a page of prose edits.

Confirming is `POST /v1/proposals/{id}/confirm`, **outside the prefix**, under
`outturn:api` -- a browser token, which the agent's turn has never held and has
no way to obtain. Only the session's owner may confirm, and the request carries a
hash of the effect the card displayed: the change and its diff, canonicalised,
not the history replay, which moves as requests arrive and is context rather
than identity. The API re-derives the effect, compares, and refuses a mismatch --
a proposal whose effect moved since somebody read it is a different proposal --
and checks the confirming person holds what the change needs *now*. Creating a
proposal already required the manage token's subject to hold it, so a proposal
nobody could confirm is never shown. Proposals expire in an hour, because a yes given tomorrow to something
proposed today is a yes to a preview that may no longer be true.

Some things are not proposable at all. Sealing a credential needs the secret,
which must never pass through a model's context: the agent may *propose the
binding* and leave a card that opens the sealing form, where the person pastes
the key into the page. The agent chooses only which integration the form is
for. The hosts, header and token URL come from what the operator's published
skill declares, or are typed by the person; anything no installed skill declares
is flagged, with its registrable domain shown, because the one field a hijacked
agent would want to fill in is where the key goes. And changing who holds `approvals:delegate`, or anything
in the roles that would widen the manage set, is not exposed under the prefix,
so a chain of proposals cannot be used to make the next one easier to confirm.

## Reads, and what they bring with them

Reads go straight through: skills and their versions, policies and their
evidence, usage, the queue, sessions. Two things follow from the agent being
able to read them.

**Transcripts are untrusted input.** A support conversation the admin agent
reads to explain a failure may contain somebody's injected instruction, now
inside a context that can propose policies. The proposal-and-confirm rule is the
defence, and it is why it must hold without exception: the worst a hijacked
admin agent can do is propose something wrong to somebody who is reading what
they confirm.

**What it reads, it can repeat.** A management turn's reply is visible to the
administrator, which is fine -- they could read it themselves. It is not to
anyone else. A management session is its owner's on **every** path -- messages,
lists, events, activity, titles, files, send, retry and cancel -- enforced where a
session is looked up, by the agent's kind, rather than handler by handler. Today
any member who may read sessions reads all of them, and some may send into one;
left like that, a viewer reads an administrator's management session and an
operator drives it.

## The handlers are thin

The manage routes overlap with the ordinary API -- listing skills, publishing a
version, reading usage -- and should not duplicate it. Each is a thin handler
over the same functions the ordinary route calls, differing in how the caller is
authorised and in writing a proposal where the ordinary route would write the
change. One rule, two doors.

## Not settled

- **Which agent this is.** An operator-provided agent every workspace gets, or a
  capability any agent can be granted. The first is simpler to reason about and
  keeps the manage rule out of agents that do other work, which is the reason to
  start there.
- **What is in the manage set.** Skills, annotations, auto-approval policies,
  usage and the queue are the obvious start. Members and roles are the obvious
  exclusion. Egress rules sit between: proposing a host is useful and is also
  the one change most worth an injection's effort.
- **A proposal from a trigger.** A scheduled check that finds a policy worth
  proposing -- "this gate has been approved unchanged three hundred times" --
  has no administrator in the session. It could raise an action item instead of
  a proposal, pointing at a management session to discuss it.
