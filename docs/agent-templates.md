# Agent templates

How an operator defines an agent once and has it in every workspace that
should have it. Built.

## Why

outturn is meant to be turnkey. An operator runs a product their customers
already use -- an accounting service, say -- brings those customers on as
workspaces, and gives them agents that work against their accounts through
the operator's integration. Today an agent is a row in one workspace, made by
hand, so an operator with five hundred customers has no way to give each of
them an invoicing agent except to make five hundred, and no way to fix a
mistake in its instructions except to fix it five hundred times.

A template is an agent the operator defines once. Each workspace that has it
gets an agent of its own made from it, and what the operator changes in the
template reaches every one of them that has not chosen otherwise.

## What a template is

A template is the operator's, as an operator's skills are -- its rows belong to
no workspace (`agent_templates`, migration 0032) -- and holds what an agent is
made of:

- name, slug and description;
- the system prompt, in two parts (below);
- the skills it is given, each following or pinned as an agent's skills are;
- settings it fixes, such as a temperature the operator certified;
- its policy, and the tools offered from the first round ([below](#eager-tools));
- three rules for the workspaces that get it: its availability, whether they
  may add to its prompt, and whether they may pin a version.

Publishing a template writes an immutable version, as publishing a skill
does. What a workspace's agent runs is always some version of it, which is
what makes following and pinning mean anything.

## An agent of each workspace's own

A workspace's agent made from a template is an ordinary agent row with a
`template_id` on it. It has its own id, and so its own sessions, its own
`agent/` storage scope, its own schedules and triggers -- everything keyed on
the agent stays inside the workspace, as it does now.

The obvious alternative, one agent in the platform workspace that every
workspace talks to, is ruled out by exactly that: the agent's id is what
scopes its files, and its sessions and triggers hang off its row. One row
shared by every tenant would be one `agent/` scope shared by every tenant.

## Availability

Each template says how workspaces come to have it:

| | What happens | For |
|---|---|---|
| **Required** | Made in every workspace, new ones included. A workspace cannot remove it | Agents the product does not work without |
| **Default** | Made in every workspace. Its admin may remove it, and add it back | Agents nearly everyone wants |
| **Optional** | Offered in a catalog; a workspace's admin adds it if it fits | Agents only some businesses want |

Accounting software is the example. Every customer gets the invoicing agent;
most get the bookkeeping one; only customers who sell goods want the
inventory agent, and a consultancy should never see it unasked.

A workspace's admin sees the default and optional templates as a catalog --
which they have, which they could add -- and adds or removes from it. Making a
template required, or creating a workspace, makes the agents the workspace is
owed; removing a default one is remembered, so a later publish does not put
it back.

## What a workspace may change

What an operator certified and what a business does its own way are both
real, and they are different parts of one prompt. So a template's prompt is
written in two parts, and a workspace may add a third:

```
## Requirements
What every workspace's agent must do. No workspace changes these.

## Defaults
How the operator expects most businesses to work. A workspace may change these.

## From this workspace
How this business works. Where it differs from the defaults above, follow it.
Never set aside the requirements.

Requirements still apply: <the requirements, restated in a line>
```

The order is deliberate. A model reads its prompt one token at a time, each
able to look only at what came before it, so the workspace's text and the
line saying how it relates come after both of the operator's parts, where
they can be read with those in view. The closing restatement is the cheap
form of what [prompt repetition](settings.md#prompt-repetition) does for a
user's message: the requirements read last, which matters most for a model
whose attention holds a long prompt imprecisely.

A workspace's text should say what it replaces -- "instead of raising suspected
duplicates, void them" -- so the model has nothing to reconcile. And nothing
that has to hold is held by the prompt alone: whether voiding an invoice needs
a person's word is an approval ([approvals.md](approvals.md)), changed by the
workspace's auto-approval policy, not by anything it writes here.

The workspace's part is bounded, as a personality is, and counted against
`context_budget`. An operator who certified an agent's exact behavior turns
additions off for that template, and the third part is never composed. It is
one more prompt contributor ([prompt-contributors.md](prompt-contributors.md)),
placed after the template's own text.

Skills and settings follow the same idea. A template's skills come with it,
composed ahead of the agent's own, and are not among the bindings a workspace
edits, so it may give its agent more and cannot take the template's away. A
template's fixed settings are the last level of the settings walk, after the
agent's own ([settings.md](settings.md)), so they win: a value a workspace or
agent could change is not one the template fixed. An agent-level override of a
fixed setting is refused, and the agent's settings page shows it as the
template's.

## Versions

An agent made from a template **follows** it by default: each turn is prepared
from the template's current version, so an operator's fix reaches every
workspace at once, and a conversation already running picks up a changed
prompt at its next compaction, as a changed personality does.

A template may **allow pinning**. A workspace that has certified its own
process against today's agent pins it to this version, sees that a newer one
exists, and moves when it chooses. An operator who needs every workspace on
the current version leaves pinning off, and required templates usually will.

A pin is `agents.template_version_id`, beside `template_id`, honored only while
the template's `allow_pinning` is on. Everything that reads a template agent's
version reads the pinned one -- its prompt and policy, eager tools, skills and
fixed settings -- while its name follows the newest, since that is how the
workspace recognizes it. Turning pinning off brings every agent back to the
newest without touching their rows, so turning it on again restores the pins.
A workspace chooses on the agent's page, which says when a newer version is
available.

## Eager tools

Tools are offered to a model in two ways: eagerly, with their full description
on every round, or deferred, by name only until the model loads them. Today
the host sends no eager list, so every tool is deferred and a model's first
round on most turns is spent loading the one it always uses.

The operator knows which tools an agent uses on most turns, because the
operator made the agent for a job. So the eager list is part of the template,
carried to the guest through `host::eager_tools` as the deployment's choice,
which is what that import was for. A workspace does not change it: which
tools an agent reaches for is a fact about the agent the operator built, and
getting it wrong costs a round or some tokens, never access to anything.

An agent made without a template has no one to choose for it, and keeps
deferring everything.

## Who may do what

- Making, publishing and retiring templates is the operator's: a system
  administrator, as for platform skills and platform settings
  (`/v1/platform/agent-templates`).
- Adding and removing a default or optional template's agent is a workspace
  admin's, as making an agent is now.
- Writing the workspace's part of a prompt, and pinning, belong to whoever may
  edit that agent.

## Retiring a template

A template retired stops being offered and stops being made in new
workspaces. The agents already made from it are not deleted -- their sessions
are the workspace's record -- but stop following, left on the last version
they ran until a workspace removes them.

## Not settled

- Whether a workspace may rename its agent, or only describe it. A name a
  customer chose is friendlier; a name the operator's support can recognize is
  easier to help with.
- What an agent made by hand before templates existed becomes. Nothing, is the
  likely answer: it stays as it is, and templates are for agents made from now.
- How well a given model keeps requirements and defaults apart is a question
  for evaluation against that model ([skill-evaluation.md](skill-evaluation.md)),
  not one this layout can answer for it.
