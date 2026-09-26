-- Action queue -----------------------------------------------------------------

-- What is waiting for a person to do something about it.
--
-- Materialised rather than derived. The queue is read on every page load by
-- every signed-in user, and the derived form -- pending items joined against
-- the reader's current role memberships -- is a join whose cost is paid by the
-- reader, repeatedly, for an answer that changes rarely. Here the cost is paid
-- once by whoever caused the change.
--
-- The price of materialising is that every path which changes the answer must
-- say so. That is what `targeted` delivery below is for: a caller that knows
-- which entries changed names them, and only a caller that cannot work it out
-- falls back to recomputing a whole target's queue.
create table action_items (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    id           uuid        not null,

    -- What this is about. Free text rather than a check constraint for the
    -- same reason `role_authorities.authority` is: the vocabulary belongs to
    -- the code and changes with it.
    kind         text        not null,

    -- What produced it, when something did. Null for an item raised directly.
    --
    -- Not a foreign key: `events` is swept (chat.delta after a day) and an
    -- action item must outlive the event that caused it. A dangling reference
    -- here is expected and means only that the originating event has aged out.
    event_id     uuid,

    -- Everything the queue renders without going back to the source. Kept
    -- here rather than joined so a queue read touches one table.
    payload      jsonb       not null default '{}'::jsonb,

    -- Open until somebody settles it. Resolution is recorded rather than
    -- deleted so a queue that emptied can still explain why.
    state        text        not null default 'pending'
                             check (state in ('pending', 'resolved', 'cancelled', 'expired')),
    resolved_by  uuid                 references users (id) on delete set null,
    resolved_at  timestamptz,

    created_at   timestamptz not null default now(),
    expires_at   timestamptz,

    primary key (workspace_id, id)
);

-- Who each item is waiting on.
--
-- Split from the item because an item may be addressed to a role, to a person,
-- or to both, and because the answer to "whose queue is this in" changes
-- without the item changing -- somebody joins the role, somebody leaves it.
--
-- A target is a role or a user, never both: the two columns are exclusive and
-- the check enforces it. Storing the role rather than expanding it to its
-- members at write time is deliberate. Expansion would freeze a snapshot of
-- membership into rows whose whole purpose is to outlive the moment: a person
-- who joins the role tomorrow would never see an item raised today, a person
-- who leaves would keep one, and an item raised while the role is empty would
-- resolve to nobody and be silently lost.
create table action_targets (
    workspace_id uuid not null,
    item_id      uuid not null,

    role_id      uuid,
    user_id      uuid references users (id) on delete cascade,

    created_at   timestamptz not null default now(),

    -- Exactly one of the two. `unique` over a nullable column treats nulls as
    -- distinct, which would let the same role be added twice, so the identity
    -- is the coalesced pair instead -- see the two partial indexes below.
    check ((role_id is null) <> (user_id is null)),

    foreign key (workspace_id, item_id)
        references action_items (workspace_id, id) on delete cascade,
    -- Composite, so a target cannot name another workspace's role. Dropping
    -- the role drops the targeting with it; the item survives, and an item
    -- left with no targets is one nobody is waiting on -- which `orphaned`
    -- reports rather than hides.
    foreign key (workspace_id, role_id)
        references roles (workspace_id, id) on delete cascade
);

create unique index action_targets_role_idx
    on action_targets (workspace_id, item_id, role_id) where role_id is not null;
create unique index action_targets_user_idx
    on action_targets (workspace_id, item_id, user_id) where user_id is not null;

-- The read path: everything targeted at one role, or at one user, newest last.
create index action_targets_by_role_idx
    on action_targets (workspace_id, role_id, item_id) where role_id is not null;
create index action_targets_by_user_idx
    on action_targets (workspace_id, user_id, item_id) where user_id is not null;

-- Counting the queue is the hot read -- a badge on every page load -- and it
-- only ever counts pending items. Partial, so the index holds the open ones
-- rather than every item the workspace has ever settled.
create index action_items_pending_idx
    on action_items (workspace_id, id) where state = 'pending';

-- The sweep for items that aged out. Partial for the same reason: an expired
-- or resolved item is never a candidate again.
create index action_items_expiry_idx
    on action_items (expires_at) where state = 'pending' and expires_at is not null;

-- Driving the global read from the item side.
--
-- The per-workspace reads start from a role or a user and find their items, and
-- the two indexes above serve that. The notification centre asks the opposite
-- question -- for each pending item in the workspaces this person belongs to,
-- is any of its targets one of theirs -- and neither of those indexes answers
-- it, because both lead with the target rather than the item.
--
-- Covering, so the check is an index-only probe: `where` supplies the workspace
-- and item, and both target columns are read without touching the heap.
-- Measured at 346k items it is what turns the targets side of that read from a
-- sequential scan into a probe per candidate.
create index action_targets_item_idx
    on action_targets (workspace_id, item_id, role_id, user_id);
