-- A model that tracks "Latest": the newest version in its line at its
-- provider, recomputed whenever a catalog is kept (docs/models.md, "Tracking
-- the latest version"). `model` stays what is asked for; these say how it got
-- there and what is waiting.
alter table models
    -- The family it tracks. Null for a pinned model, which never moves on
    -- its own.
    add column tracks text,
    -- A newer version waiting on somebody: for a tracked model, one that a
    -- value set for it refuses; for a pinned one, any. With why, and the
    -- adjustments approving it would make to those values.
    add column pending_model text,
    add column pending_reason text,
    add column pending_adjustments jsonb,
    add column pending_since timestamptz,
    -- What it asked for before its last move, and when it moved, so a turn
    -- begun before the move finishes on the version it began on.
    add column previous_model text,
    add column previous_facts jsonb,
    add column changed_at timestamptz,
    add check ((pending_model is null) = (pending_since is null));
