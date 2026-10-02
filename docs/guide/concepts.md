# Concepts

The parts of outturn and the words the rest of these pages use. Each section
links to the design doc that goes deeper; the [glossary](../glossary.md) has
every term in one place.

## Three tiers

outturn is three services, and the split is a security boundary rather than a
packaging one.

| Tier | Holds | Does |
|---|---|---|
| **api** | The signing key and the database | HTTP API, sign-in, transcripts, the job queue |
| **gateway** | The credentials | Talks to model providers, and makes an agent's outbound requests |
| **runtime** | Nothing | Runs agent code in a WebAssembly sandbox |

An agent runs in the runtime with no filesystem, no sockets and no
credentials. Everything it can do is a host function declared in
`wit/agent.wit`. When it wants a model, it calls `chat` and the host attaches
the key. When it wants a URL, the gateway fetches it. So a compromised agent
can spend its own session's allowance and nothing more.

## Workspaces

A **workspace** is a tenant, whatever a tenant is in your product, and it is
the isolation boundary: every row, file and model call belongs to exactly one.
People sign in as **workspace members** holding **roles**, and each role
bundles fixed **authorities** such as `sessions:read`. The **operator** runs
the deployment and holds platform-wide authorities no workspace can grant
itself.

See [Tenancy, credentials and attribution](../workspaces.md) and
[Authorities and roles](../authorities.md).

## Agents, sessions and turns

An **agent** is a system prompt, a model and the skills it is given. A
**session** is one conversation with it. Each message starts a **turn**: one
pass of the agent loop, queued as a **job** and taken by whichever runtime pod
has room. A turn may take many rounds of model call and tool call before it
answers, and the browser sees each round as it streams.

A turn can be stopped mid-way, or held: an **inhibitor** such as a spend cap,
an operator's stop, or an **approval** waiting on a person. A held turn
**parks** and resumes once the hold is lifted. See
[Inhibitors](../inhibitors.md) and [Approvals](../approvals.md).

## Skills

A **skill** is instructions an agent is given beside its system prompt. A
**package** adds files the agent reads only when it needs them, as
`skill/<slug>/<path>`. That is how an API with hundreds of operations fits: a
short manifest in the prompt, and a file per operation read on demand. The
[OpenAPI wizard](../openapi-wizard.md) generates such a package from a
specification.

## Reaching the outside

An agent reaches nothing by default. An **egress rule** names a host one
workspace's agents may reach, and the gateway checks every request against the
workspace's rules, then checks the address the name resolves to: an allowed
name that resolves inside the cluster is still refused. Redirects are not
followed.

A rule can carry a credential, named rather than stored: the rule holds the
name of an environment variable on the gateway, and the gateway attaches the
value on the way out. The agent never sees it. Each such variable is bound by
the operator to the workspaces and hosts it may be used for.

See [Egress](../egress.md), [Integrations](../integrations.md) and
[Credential bindings](../credential-bindings.md).

## Usage

Every model call is a row in the usage ledger, tagged with its workspace,
agent, session, user, the model that actually served it and whose key paid.
`/v1/usage` exports it, and bills are built from that export. See
[Usage](../usage.md) and [Routing](../routing.md).
