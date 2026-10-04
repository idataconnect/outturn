-- A generated skill as a derivation: the specification it was made from, kept,
-- and what people added to it, kept apart from what generation produces -- so
-- that an updated specification proposes a new version that keeps them. See
-- docs/openapi-wizard.md, "A derivation, not an output".

-- Where a skill is generated from. One per skill; a skill without one is
-- written by hand, or was detached from its source.
create table skill_sources (
    skill_id uuid primary key references skills(id) on delete cascade,
    base_url text not null,
    -- The header the credential travels in, as the wizard's page confirmed it.
    auth_header text,
    -- The URL it was fetched from, if it was, so a refresh can fetch again.
    fetched_from text,
    created_at timestamptz not null default now()
);

-- Each specification a skill was generated from. The document itself is in the
-- object store by hash, beside the skill's files, under the owning workspace.
create table skill_source_revisions (
    id uuid primary key,
    skill_id uuid not null references skill_sources(skill_id) on delete cascade,
    spec_sha256 text not null,
    spec_bytes integer not null,
    created_by uuid,
    created_at timestamptz not null default now()
);

create index skill_source_revisions_skill_idx on skill_source_revisions (skill_id, id desc);

-- What people added, keyed by what the specification names -- the whole skill,
-- a category by its tag, an operation by its name -- and never by a file path,
-- since files are what generation produces. Retired rather than deleted, so a
-- version can say which annotations it was made with.
create table skill_annotations (
    id uuid primary key,
    skill_id uuid not null references skills(id) on delete cascade,
    level text not null check (level in ('skill', 'category', 'operation')),
    target text,
    kind text not null check (kind in ('note', 'prefer', 'hidden', 'approval')),
    -- The note, the preferred operation's name, or the approval rule's YAML.
    -- Empty for hidden.
    value text not null default '',
    created_by uuid,
    created_at timestamptz not null default now(),
    retired_at timestamptz,
    check ((level = 'skill') = (target is null)),
    -- Only a note means anything above one operation.
    check (kind = 'note' or level = 'operation')
);

create index skill_annotations_skill_idx on skill_annotations (skill_id, id)
    where retired_at is null;

-- Which specification and which annotations a version was generated from, so a
-- reader can tell a derived version from a hand-written one and what made it.
alter table skill_versions
    add column source_revision_id uuid references skill_source_revisions(id),
    add column annotation_ids uuid[];
