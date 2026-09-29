# Skill bundles

Several skills shipped, enabled and versioned as one thing, because the job
they do together is the thing somebody actually wants.

Nothing here is built. It rests on skill packages
([skill-packages.md](skill-packages.md), built but for the UI) and overlaps
heavily with integrations ([integrations.md](integrations.md), designed and
unbuilt) -- a bundle without the hosts and credentials its skills need is a
bundle that cannot do its job, so the two arrive together or the first is a
demo.

**This term used to mean the other thing.** Until recently "bundle" meant one
skill published as a body plus its files, and the glossary warned against
reading it as a set of related skills. That was the wrong way round: people
kept reading it as a set because a set is the thing they wanted. The one-skill
concept is now a *package*, and this is a bundle.

## What somebody is buying

"Accounts Receivable" is not a skill. It is chasing an overdue invoice, taking
a payment, applying a credit note, and knowing which of those needs a person to
say yes -- four or five skills, the host they all talk to, the credential bound
to it, and the approval rules over the two that move money.

Today a workspace assembles that by hand: create each skill, write each body,
add the host to the egress rules, bind the credential, attach them to an agent
in the right order, and remember to do it all again on the next agent. Every
step is a place to stop halfway, and a half-installed bundle is worse than none
-- an agent that can take a payment but not apply a credit note will take
payments it should not.

So a bundle is an installable unit with a name a person recognises, and
installing it is one decision rather than a dozen.

## What is in one

A bundle names:

- **Its skills**, each a package in its own right, in the order they should be
  given to an agent. Order matters: skills compose into one system prompt and
  the earlier ones set the terms the later ones use.
- **The hosts** its skills reach, which become egress rules the workspace must
  approve. A bundle cannot grant itself reach -- see below.
- **The credentials** those hosts need, by name rather than by value, the way
  an egress rule already names an environment variable
  ([egress.md](egress.md)). Installing a bundle tells a workspace which
  credentials it must supply; it never carries one.
- **Its approval rules**, which already live in the skill files as frontmatter
  ([approvals.md](approvals.md)) and therefore need nothing new here. Worth
  stating because it is the part people expect to be a bundle-level setting and
  it should not become one: an approval is about an operation, and the
  operation is in a skill.

A bundle is *not* a skill. It has no body, composes into no prompt, and an
agent is never given "a bundle" -- installing one gives the agent its skills,
individually, in `agent_skills`, exactly as today. That matters because
everything downstream already works on that table: ordering, disabling one
skill, overriding one, the stats panel. A bundle that introduced a second way
to attach a skill would need all of it again.

## Installing is a proposal, not an act

The dangerous shape is a bundle that installs itself: reaches a host, takes a
credential, and starts answering customers because somebody clicked a name in a
catalogue.

So installing a bundle produces a *plan* a person approves: these skills will
be created, this host will be added to your egress rules, these credentials are
needed and here is what each is for, these two operations will require an
approval. Nothing happens until somebody says yes to the whole of it, and the
egress rules go through the same path a hand-written one does. The rule that a
workspace which has not thought about a host has not consented to it
([egress.md](egress.md)) is not softened because the host arrived in a bundle.

That also gives the honest answer to the half-installed problem: the plan is
applied in one transaction, or it is not applied.

## Versioning is the hard part

A skill package is versioned, immutably, and an agent binds to a version or
follows the latest. A bundle has to have a version too -- "Accounts Receivable
2.1" is the thing somebody installed and the thing they will ask about -- and
the two must not disagree.

The shape that seems right: **a bundle version names an exact version of each
of its skills.** Installing bundle 2.1 creates or updates the skills to the
versions it names. Upgrading to 2.2 is a plan like the first install, listing
what changes. A workspace that has edited one of the skills has forked it
(`fork` already exists), and the upgrade plan says so rather than silently
overwriting their work.

What is genuinely unsettled:

- **Whether a bundle's skills are the workspace's to edit.** Editing one is
  what makes the next upgrade ambiguous. Refusing the edit is what makes people
  fork the whole bundle to change one sentence.
- **What happens to an agent mid-upgrade.** A turn is running against the
  version it resolved at its start; the version pinning in `agent_skills`
  covers that, but "upgrade the bundle" and "and repoint every agent" are two
  decisions and the second is not obviously the platform's.
- **Whether a bundle can be partially enabled.** Somebody who wants the
  chasing but not the taking-payment will ask. Saying yes means a bundle is a
  menu rather than a unit, and the coherence argument above starts to fall
  apart.

## Who publishes one

Three sources, in the order they are likely to matter:

- **The operator**, shipping the bundles their deployment is about. This is the
  case worth building for: an operator who knows the domain writes the bundle
  once and every workspace installs it.
- **A workspace, for itself**, promoting an assembly it built by hand into
  something it can put on a second agent. This is the cheapest useful version
  and may be the right first step.
- **A third party.** Everything above is about containment for this reason, but
  it is the least urgent: a marketplace is a business decision rather than a
  technical one, and nothing here forecloses it.

## What this needs that does not exist

- A bundle and bundle-version table, and the plan that installing one produces.
- Some way for the egress rules a bundle names to be reviewed and applied with
  it, which is the integrations design from the other end.
- Skill packages' UI, since a bundle is mostly a way of handling several of
  them and there is no screen for one yet.

## Open questions

- Is a bundle a versioned artefact or a recipe? The above assumes the first.
  The second is cheaper and loses the ability to say what somebody is running.
- Does a bundle carry an agent's system prompt, or only skills? Domains have
  opinions about how the agent should behave, not only what it can do -- but a
  bundle that rewrites the prompt is much harder to install beside another.
- What does uninstalling do to the transcripts? A skill a bundle removed still
  appears in the history of every turn it served, and `turn_skills` points at
  versions that must not disappear.
