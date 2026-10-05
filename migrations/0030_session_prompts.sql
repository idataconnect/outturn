-- What a conversation's system prompt was composed from, and the prompt
-- itself, kept so every turn of the conversation sends the same one.
--
-- Composed at a conversation's first turn and again only at compaction -- the
-- moment the prefix changes anyway -- so a provider's prompt cache keeps
-- hitting from one message to the next, and an edit to an agent or a skill
-- reaches a running conversation when it next compacts rather than mid-thought.
-- See docs/prompt-contributors.md. Gates are not frozen with it: they are read
-- from the live versions every turn, so tightening one reaches every
-- conversation at once.
create table session_prompts (
    session_id  uuid        primary key references agent_sessions (id) on delete cascade,
    -- The model the preamble names. A different model recomposes, or the
    -- agent would be told it is something it is not.
    model       text        not null,
    prompt      text        not null,
    -- The skills as resolved when it was composed: versions, bodies and the
    -- files a turn may read, so the prose and the files agree.
    skills      jsonb       not null,
    composed_at timestamptz not null default now()
);
