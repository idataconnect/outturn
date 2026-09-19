# Authorities and roles

Who may do what, and where each half of that answer lives.

## Two vocabularies

**Authorities** name actions the code can take: `agents:create`,
`sessions:read`, `storage:workspace:write`. They are an enum in
`src/auth/rbac.rs`, and every handler checks one by name. The list changes when
the code changes and nowhere else, because an authority nothing checks is
noise and an action nothing names cannot be granted.

**Roles** bundle authorities under a name a person recognises. For workspaces,
roles are data: rows in `roles` and `role_authorities`, owned by the workspace,
created and edited by whoever holds `roles:manage` there. Every workspace starts
with copies of the three defaults in `rbac::DEFAULT_ROLES` -- admin, operator,
viewer -- and may rename them, change what they bundle, or replace them. An
enterprise customer whose vocabulary is "analyst" and "compliance reviewer" is
not a special case; it is three rows with different names.

The platform reserves a few roles for itself, and those are not rows:
`system_admin`, `runtime`, `turn`. They stay in the `Role` enum because nobody
else may define what they mean, and the role store refuses to create a workspace
role with one of their names.

## What a token carries

Role names. Not authorities, and not a resolution of them.

A token that carried authorities would grow with every one a role bundled, and
a role edit would take effect only when tokens expired. A token that carries a
handful of role names stays small however many authorities they stand for, and
means whatever the workspace's rows say at the moment of the request. Editing a
role therefore takes effect on the holder's next click.

The one lag that remains is membership: changing *which roles a person holds*
reaches them at their next refresh, up to fifteen minutes, because the names
are in the token. That is the same lag the code always had. If it ever matters,
the API can look membership up per request too, and the names in the token
become a hint rather than the truth.

## Resolution, per request

`api::router::authorities_of` unions two sources:

- `rbac::platform_authorities` for platform roles, in code. This is all the
  gateway ever needs, because the only role a token it accepts can carry is
  `turn`, and the gateway has no role store.
- `RoleStore::authorities_for` for workspace roles, from the rows. Cached per
  workspace in each API pod. A write to a workspace's roles announces itself on the
  `outturn_roles` Postgres channel and every pod drops that workspace's entry, so
  there is no TTL to guess at.

## Guards, because roles are editable now

A role may bundle only authorities that are *workspace-assignable*. Platform-wide
authorities -- `workspaces:*`, `work:take` -- are refused, whoever asks. This is
the successor to the check constraint that used to pin role names: a workspace
that could grant itself `workspaces:create` could make workspaces.

A role editor may bundle only authorities they hold themselves. Otherwise
`roles:manage` is a ladder: define a role with everything, grant it to
yourself, climb.

A role somebody holds cannot be deleted. Take it away from them first.

## Membership is a separate question

Which roles a person holds in a workspace is answered today by
`user_workspace_roles`, and by nothing else. It is deliberately not the same
thing as what a role means, so a second source can be added later: groups or
claims from an identity provider, arriving at login and mapped onto local roles
through a workspace-scoped table. Authorities would still come only from the
workspace's own rows; an external system could say "this person is in group X",
never "this person may do Y".

## Narrowing an authority to some agents

`user_agent_scopes` holds the grants, `ScopeStore` resolves and caches them, and
`require_for_agent` is the guard. Starting a conversation is narrowed, and so is
everything done to one afterwards: reading it, renaming it, deleting it, sending
into it and stopping its turn. The session listing filters rather than refusing.

A write is narrowed wherever its read is. The guard was opt-in per handler once,
and four of them did not opt in -- so a person narrowed to the support agent was
refused the accounting agent's transcripts and could still delete them, rename
them and post into them, with nothing between them and it but knowing an id, and
ids travel in URLs. `sessions::session_for` now loads the conversation and
proves the authority together, so a handler cannot obtain the session without
naming what it is acting under, and the next write endpoint inherits the guard
instead of having to remember it.

Having started a conversation settles most of what can be done to it: your own
history is yours to read, rename, delete and stop. It does not settle sending.
A narrowing says which agents a person may use, and somebody who keeps an old
thread open keeps using one -- its tools, its skills, the workspace's allowance
-- so an exemption there would narrow the roster and nothing else. Stopping a
turn stays exempt because it is the safe direction: refusing it would leave
somebody watching an agent they cannot reach spend the allowance.

The event feed narrows too, and has to. `/v1/events` carries the conversation as
it is written -- the same words the transcript endpoint serves, arriving a
little earlier -- so a narrowing that stopped at the transcript would refuse the
history and stream the present. The filter is applied inside the query rather
than to the rows that come back: a page filtered afterwards can come back empty
and read as "nothing yet", which for a long poll is a request that returns
instantly and forever. A caller narrowed to nothing in a window is given a
watermark instead, so their cursor still moves past what they could not see and
the next poll does not rescan the same span.

An agent's files narrow with its conversations -- a transcript says what was
said and a file is what somebody uploaded, both being what the agent has done --
and the file listing leaves out a scope it may not read rather than refusing the
call. The guard sits in `files::space_for`, which is the one place that knows
which agent a session belongs to.

`GET /v1/scopes` lists who is narrowed and to what; `PUT /v1/scopes/{user_id}`
replaces one person's set, and an empty list restores them to the whole
workspace. Both need `roles:assign`: deciding whose reach is narrowed is the
same kind of act as deciding what roles they hold, and an operator who builds
agents is not the person to make it. The user editor shows it as a list of
agents to tick, absent for everybody until somebody ticks one.

Only the narrowed are listed. Showing every unnarrowed person as "all agents
ticked" would make the ordinary case look like a decision somebody took.

An authority is a workspace-wide statement: holding `sessions:read` reads every
conversation with every agent in the workspace. That is right for a workspace
whose agents are all the same business, and wrong for one running an accounting
agent beside a support agent, where the people who should read one have no
business reading the other.

**The line is existence against contents.** Which agents exist is
workspace-public -- an admin has to see what is running to administer it, and a
roster is not a leak. What an agent has *done* is not: a transcript is what was
said, an agent's files are what somebody uploaded, and an approval request
carries the thing being approved. So `agents:read` stays workspace-wide, and
three authorities gain a per-agent narrowing:

| Authority | Narrowed | Why |
|---|---|---|
| `agents:read` | no | The roster is what an administrator needs |
| `sessions:create` | yes | Who may talk to this agent at all |
| `sessions:read` | yes | Who may read *other people's* conversations with it |
| `storage:agent:read` | yes | An agent's files leak separately from its transcripts |

Writes narrow with their reads -- `sessions:delete` and `storage:agent:write`
against the same grant -- because somebody who cannot read an agent's
conversations should certainly not be able to delete them.

**Your own conversations stay yours.** `agent_sessions.user_id` already records
who started one, so "may talk to an agent but not read what others said to it"
is expressible without a new column, and is the ordinary shape: a person uses
an agent and sees their own history, while reading everybody's is a separate
grant.

**A grant narrows; no grant means the workspace-wide authority stands.** A
deployment that has never used this behaves exactly as it does today, and the
migration is a no-op. The alternative -- no grant meaning no access -- would
lock every existing workspace out of its own data on upgrade to buy a default
nobody asked for.

**The grant is per person, not per role.** Two support leads holding the same
role may cover different agents: the job is the same and the patch differs.
Putting it on the role would mean a role per patch, which is how a role list
becomes unreadable. So a table keyed `(workspace_id, user_id, agent_id)`,
consulted only for the narrowed authorities.

**Nothing about the token changes.** Authorities are already resolved per
request rather than minted into the token, so the scope is another lookup on a
path that is already doing one. It caches the way roles do -- per workspace, in
each pod, dropped on a Postgres notification -- and for the same reason: a
change to who may see what should take effect on the next click, not at the next
refresh.

Two checks rather than one, then. *May you do this at all* is `authorities_of`
as it stands; *for this agent* is the new grant. Listings filter rather than
refuse, so a session list returns the agents you may see instead of failing on
the first one you may not.

### Why this is the thing to build before approvals

A human-in-the-loop request carries what is being approved, which is the
payload it exists to show somebody. A support lead seeing "may I create a ticket
for this customer's medical billing dispute" has read the thing the grant was
meant to keep from them.

Routing approvals separately -- an on-call rotation, say -- would be theatre
while the same person can open the agent's sessions and read it there. So the
approval queue filters by the same predicate as everything else: the agents
whose contents you may read. Build the narrowing first and approvals inherit it;
build approvals first and the queue ships with the wrong visibility.

## Storage scopes

Files have three lifetimes (see [storage.md](storage.md)), and access to two of
them is an authority:

| Scope   | Read                   | Write                   |
|---------|------------------------|-------------------------|
| Workspace  | `storage:workspace:read`  | `storage:workspace:write`  |
| Agent   | `storage:agent:read`   | `storage:agent:write`   |
| Session | implied by `sessions:read` | implied by `sessions:create` |

Session scope needs no authority of its own: anyone who can be in a
conversation can attach a file to it and read what is in it. In the default
roles, admin holds all of these, operator reads workspace files and reads and
writes agent files, and viewer reads both.

These govern people. An agent running in the sandbox reads through host
imports with no person in the loop, so what *it* may reach is a policy on the
agent, not an authority on a user. That policy is not built yet.
