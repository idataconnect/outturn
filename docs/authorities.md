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
