-- Approval grants ---------------------------------------------------------------

-- What a yes is worth, once somebody has given it.
--
-- Without this a turn that is approved and resumes runs into the same gate and
-- is refused a second time: releasing the hold lets the job run, but nothing
-- about the request changed, so the gateway makes the same decision it made
-- before. The hold answers "may this turn proceed"; a grant answers "may this
-- request go out", and they are different questions.
--
-- A row rather than a signed capability, for the two reasons docs/approvals.md
-- gives. The wait crosses a park that may last days and a pod that may not
-- survive it, so a token would need an expiry nobody can pick honestly. And a
-- grant has to be auditable after the fact -- a standing grant nobody can list
-- is an authority the roles UI cannot see.
create table approval_grants (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    id           uuid        not null,

    -- The conversation this was granted within. A grant never leaves its
    -- session: the person who approved was looking at this conversation.
    session_id   uuid        not null,

    -- The turn it was granted for. Both extents die with the turn, so nothing
    -- accumulates and nothing granted at nine reaches a call at five.
    --
    -- Not a foreign key to jobs: a job row is the unit of work and may be
    -- retried, while the grant is about the turn a person looked at. A grant
    -- outliving its job row by a moment is expected.
    job_id       uuid        not null,

    -- The act, matching the gate's `requires`. Compared, never read: a grant
    -- for `charge` does not cover a `refund`, however close they look on
    -- screen.
    requires     text        not null,

    -- How far this reaches. `call` is the default and covers one request shape;
    -- `unit` is what a ticked `covers` grants and spans every declared act of
    -- that `requires` against one identified unit.
    --
    -- Both are bounded by the turn rather than by the call. Spending a grant on
    -- use would need a write from whoever checks it, and what checks it is the
    -- gateway, which holds no database on purpose -- so a repeated identical
    -- call within one turn is repeated approved. Stated here because the column
    -- below used to imply otherwise.
    --
    -- Wider extents -- this session, until revoked -- are deliberately absent.
    -- They are authorities rather than approvals, and the place to add one is
    -- the roles model, where a list of who holds what already exists.
    extent       text        not null check (extent in ('call', 'unit')),

    -- What the grant is keyed on, by extent.
    --
    -- For `call`: the request shape that was refused -- method, host and the
    -- normalised path -- hashed into one value. docs/approvals.md keys this as
    -- (session, turn, call-ordinal), but no call ordinal exists in this
    -- codebase: docs/idempotency.md is itself unbuilt, and inventing half of it
    -- here would be a second spelling of a design nobody has settled. The
    -- request shape dedupes the resumed turn exactly as well, because a retry of
    -- the same call is the same method, host and path by construction. What it
    -- does not distinguish is two identical calls in one turn, which is the
    -- narrow case the ordinal would buy -- recorded here so the substitution is
    -- visible rather than discovered.
    --
    -- For `unit`: the value of the field the gate's `identified_by` names, read
    -- from the request the gateway was about to make rather than from anything
    -- the guest asserted separately. A guest that could name its own unit could
    -- name the one that was already approved.
    keyed_on     text        not null,

    -- Who said yes, and when. The whole point of a row over a token.
    granted_by   uuid                 references users (id) on delete set null,
    granted_at   timestamptz not null default now(),

    -- Reserved, and written by nothing today.
    --
    -- It was added for a single-use `call` grant, spent by whoever checked it.
    -- The check then moved to the gateway -- the only tier that sees the request
    -- body, which a `unit` grant has to be matched against -- and that tier holds
    -- no database, so nothing is in a position to spend one. Kept rather than
    -- dropped because the narrower extent is still the one worth having if a
    -- repeated call proves to be a hazard, and because a column that has never
    -- held a value is cheaper to start using than to re-add.
    --
    -- Read by `live_for`, so a row that ever does get spent stops travelling.
    spent_at     timestamptz,

    primary key (workspace_id, id)
);

-- The read a turn makes when it is prepared: everything this turn still holds,
-- to be committed into its token. Not the gateway, which reads the token and
-- never this table.
create index approval_grants_turn_idx
    on approval_grants (workspace_id, job_id)
    where spent_at is null;

-- Listing what somebody granted, for the audit the row exists to support.
create index approval_grants_granted_by_idx
    on approval_grants (granted_by, granted_at desc)
    where granted_by is not null;

-- Finding the approval a turn or a conversation is waiting on.
--
-- `gated::raise` asks "does this turn already have this act pending" before
-- raising a second identical question, and `announce_hold` asks "is there an
-- approval on this conversation" for every held turn -- including the spend caps
-- and operator stops that are not approvals at all. Both reach into `payload`,
-- which no index covers by default, so both were a scan of the workspace's
-- pending items on a per-turn path.
create index action_items_pending_job_idx
    on action_items (workspace_id, (payload ->> 'job_id'))
    where state = 'pending';

create index action_items_pending_session_idx
    on action_items (workspace_id, (payload ->> 'session_id'))
    where state = 'pending';

-- What an item is waiting on ---------------------------------------------------

-- The hold an action item stands for, where it stands for one.
--
-- `docs/action-queue.md` says the queue's `event_id` should become this, and
-- that nothing should start reading `state` as the answer to "is this request
-- still open" -- the hold is that answer, and a second copy of it on the item is
-- a second thing to keep true. This is the column that makes the first half
-- possible; the lifecycle columns stay for now, because the queue still renders
-- from them and removing both at once is two changes wearing one hat.
--
-- Added because two new readers needed to ask "is there an approval on this
-- conversation" and "does this turn already have this act pending", and both
-- were answering it by extracting JSON out of `payload` -- unindexable, and on
-- the path of every held turn including the spend caps and operator stops that
-- are not approvals at all.
alter table action_items add column inhibitor_id uuid;

-- Finding the approval a conversation is waiting on, which `approval_on_session`
-- asks through the hold rather than by reading the item's payload.
--
-- Partial on `pending` because an answered item is not what it asks about, and
-- the queue's own reads are already keyed on the same predicate.
create index action_items_pending_hold_idx
    on action_items (workspace_id, inhibitor_id)
    where state = 'pending' and inhibitor_id is not null;

-- One pending approval per turn, per act ---------------------------------------

-- `gated::raise` checks for an existing approval and then takes a hold, and
-- between those two statements a second refusal can arrive -- a guest making two
-- gated calls in one round does exactly that. Both see nothing pending, both
-- take a suspended hold, both raise an item. Answering one then releases one
-- hold while the other still suspends the turn, so the conversation stays parked
-- with a second question nobody was told about and an approver who was told
-- their answer resumed something.
--
-- Made impossible rather than unlikely: the loser of the race gets a unique
-- violation, which the raise path reads as "somebody else asked first" -- the
-- answer it wanted anyway.
--
-- Keyed on the turn and the act rather than on the conversation. Per
-- conversation was the first shape and it is too wide: two different acts can
-- legitimately be pending at once -- a charge for one person to answer and a
-- refund for another -- and `/v1/approvals` raises those by hand with no turn
-- involved. Only the automatic path can produce the duplicate this prevents, and
-- it always carries a `job_id`.
create unique index action_items_one_pending_approval_per_turn
    on action_items (workspace_id, (payload ->> 'job_id'), kind)
    where state = 'pending' and payload ? 'job_id' and kind like 'approval.%';
