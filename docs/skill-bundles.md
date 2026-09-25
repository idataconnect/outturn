# Skills as bundles

A skill as a set of files rather than one body: which of them the prompt
carries, which the agent reads when it needs them, and why the set has to be
versioned as a whole.

Partly built. A version carries files: stored by hash, listed with the
version, carried forward by a body-only edit, copied by a fork, and readable
through `GET /v1/skills/{id}/versions/{version_id}/files/{path}`. An agent
cannot read them yet -- the `skill/` scope below is the next step -- so
Hollowbrook still writes its detail into `workspace/` scope.

## Why a body is not enough

A skill body is composed into the system prompt on every round of every turn
(`skill::compose`), so everything a skill says is paid for continuously whether
or not the turn needs it. For short prose that is the right trade: it is always
there and the model never has to decide to go and get it. For an API, or any
skill whose detail runs to pages, it is a cost that grows with the skill and
buys nothing on the turns that never use it.

The answer the guest already gives for its own tools is lazy loading:
`load_tools` offers names and hands over definitions when asked. The same trade
works for prose. A small **body** says what exists and where to read more; the
detail sits in **files** the agent reads with `read_object` when it decides it
needs one. [openapi-wizard.md](openapi-wizard.md) is that trade applied to a
specification, and `k8s/components/hollowbrook/skill/` is one run of it done by
hand -- a model that read a manifest line went and read the file, every time,
which is the assumption everything here rests on.

## What goes wrong when the files live outside the skill

Hollowbrook's install script puts the body in the skill and the detail in
`workspace/api/hollowbrook/`. It works, and it shows where the seams are:

- **Only half of it is versioned.** The body appends a version; the files are
  written over in place. They are not independent -- the body names operations
  and the files say how to call them -- so the history can say what the body was
  on a date and not what the files beside it said. A binding pinned to a
  version, "for a workspace that wants changes reviewed before they arrive",
  pins the half that is not the detail. The text that actually ran is recorded
  for the body and inferred for the rest.
- **Nothing owns the files.** Retiring or deleting the skill leaves them where
  they were. Forking it copies the body and not the detail, so the fork's
  manifest points at files the original can still rewrite.
- **The wrong authority edits them.** Anyone with `StorageWorkspaceWrite` can
  change what agents are told about an API, without a version, a note or the
  skill's own permissions. It is prose governing behaviour, edited through the
  path meant for reference spreadsheets.
- **The operator cannot ship one.** A skill in the platform workspace reaches
  every workspace, but its files would sit in the platform workspace's
  `workspace/` scope, which no other workspace's agent can read. A split skill
  is therefore something only a workspace can write for itself -- and the
  expected author of split skills is the operator, running the OpenAPI wizard
  once for integrations every workspace binds. This is the one that makes the
  rest urgent: under the current layout the wizard's main case cannot work.
- **They sit among a person's own files.** The files panel lists them beside
  whatever was uploaded, where they read as clutter at best and invitation to
  edit at worst.
- **Installing needs a detour.** The files endpoint lives under a session, so
  the script opens a throwaway session to upload through. Its glob also uploads
  `index.md`, so the body is stored twice and the two copies can drift.

Every one of these is the same fact: the detail is part of the skill and is
stored as if it were not.

## The shape

**A version is a body and a set of files, published together.** Appending a
version appends both; a version is immutable once written, files included. A
skill with no files is exactly today's skill, so the simple case does not
change and nobody writing three paragraphs has to know files exist.

```
skill_version_files (version_id, path, sha256, bytes)
```

Content lives by hash, so a version that changes one file of forty stores one
file, and a redeploy that changes nothing stores nothing -- the same rule the
install script applies to the body today, applied to the whole set.

**The agent reads them through a scope of their own**, `skill/<slug>/<path>`,
read-only whatever the settings say. `scope::resolve` maps it against the
version the turn actually resolved, not the live one: a pinned agent reads its
pinned files, and a turn that started before an edit reads what it started
with. That needs the turn's resolved skills to carry a slug, which
`ResolvedSkill` does not yet. A skill the turn was not bound to does not
resolve, so an agent cannot read the detail of skills it was not given.

**Stored under the owning workspace**, scope-first as [storage.md](storage.md)
requires, and untouched by the session lifecycle rule. An operator skill's
files live under the platform workspace and resolve for every workspace bound
to it, which is the thing the current layout cannot do.

**Written through the skill**, `PUT /v1/skills/{id}/versions` taking the files
with the body, under the skill authorities. The files panel does not list them;
the skills UI shows them inside the version they belong to, where a diff
between versions covers the whole of what changed.

**Forks and overrides follow.** A fork copies the file list, which costs
nothing because the content is shared by hash. An override stays prose: it
speaks about its base's files the way it speaks about its base's body, and does
not replace them. Whether an override should be able to add files of its own
is a question to answer when somebody needs it.

## When to split

Recommended past a size, not required. Splitting has a cost of its own: the
model has to decide to read a file, and a weaker one may guess instead, which
is what [skill-evaluation.md](skill-evaluation.md) exists to catch. So the
recommendation should come from numbers the platform already has rather than a
rule of thumb:

- **What the body costs.** Bytes composed into every round, shown beside the
  skill, so a body that has grown is visible as a standing cost.
- **Whether the split is working.** For a split skill, how often a turn read the
  file before making the call it describes, against how often it made the call
  without reading. That is a query over transcripts, not an inference.

A body that is large and whose sections are rarely relevant to a given turn is
the case to split. One that is large and needed on every turn is not, and
splitting it only adds a round.

## Help with splitting

Two ways in, both proposals a person reviews rather than changes that land:

- **From a specification.** The OpenAPI wizard, which produces a bundle
  directly: a manifest body and a file per operation. It should write into this
  shape rather than into `workspace/`, so it is worth building this first and
  not setting the current layout in stone.
- **From prose.** A skill whose body has grown can be offered a split: a model
  reads it and proposes a body naming the sections and a file for each. A
  one-off task at edit time, not on the turn path, so it can afford a capable
  model. The proposal is a new version, so accepting it is reversible and the
  history says it happened.

The same rule as the wizard's applies to the files a split produces: each
readable in one `read_object` call, named so the path can be derived from the
body, and no URL in the body for a weaker model to mistake for a tool name.

## What this changes for Hollowbrook

Its install script uploads the files with the version instead of through a
throwaway session, drops `index.md` from the file set because it is the body,
and the manifest's paths become `skill/hollowbrook/<operation>.md`. Nothing
about the prose changes, which is the point: the shape was right, and only
where it was stored was not.
