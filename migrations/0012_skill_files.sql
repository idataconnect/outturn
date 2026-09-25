-- The files a skill version carries beside its body, read by an agent when it
-- needs them rather than composed into every prompt (docs/skill-bundles.md).
--
-- Part of the version, and as immutable as its body: a version is everything
-- the agent was told, so the history has to say what the files said too.
--
-- The content is in the object store, keyed by hash under the workspace that
-- owns the version, so a version that changes one file of forty stores one.
create table skill_version_files (
    version_id uuid not null references skill_versions (id) on delete cascade,
    -- Relative to the skill, as the agent names it after `skill/<slug>/`.
    path       text not null,
    sha256     text not null,
    bytes      int  not null,
    primary key (version_id, path)
);
