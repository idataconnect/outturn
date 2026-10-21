-- Held requests ------------------------------------------------------------------

-- A request refused for want of an approval, kept whole so that what a person
-- approves is what goes out. See docs/approvals.md, "The approval carries the
-- call".
--
-- Until this, a refusal kept only the shape a person needs to decide -- method,
-- host, path and the bound fields, on the queue item -- and the resumed turn was
-- told to make the call again. The model then wrote the request afresh from its
-- transcript, and a grant keyed on the digest of what was refused stopped
-- matching the moment it rounded an amount. Held here, the bytes refused are
-- the bytes sent, so the grant matches by construction.
--
-- Beside the item rather than in its payload: the payload is what every target
-- of the item may see, and a body can carry more than a person deciding needs.
-- Nothing in it is a secret the platform issued -- the gateway attaches the
-- credential and refuses one the guest set -- but it is the workspace's data,
-- so it lives as long as the item and goes with it, or with its conversation.
create table held_requests (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    id           uuid        not null,

    -- The question a person is asked about it. One per item: a second refusal
    -- of the same call while the first is pending is answered by the first.
    item_id      uuid        not null,

    -- The conversation it was refused in, and the turn. The turn is a record,
    -- not a key: what makes this good for a later turn is that it is keyed to
    -- the request, so a turn that ended badly does not take the yes with it.
    session_id   uuid        not null references agent_sessions (id) on delete cascade,
    job_id       uuid        not null,

    -- The gate it was refused by, and the digest a `call` grant for it is keyed
    -- on. What the turn sending it carries in its commitment, so the gateway
    -- lets exactly this request through.
    requires     text        not null,
    shape        text        not null,

    -- The skill operation it was, where the guest said, so sending it is
    -- claimed against the idempotency record as the guest's call would be.
    operation    text,

    -- The request as the guest made it, with any idempotency key the host
    -- added: method, the URL with its query, the guest's headers and the body.
    method       text        not null,
    url          text        not null,
    headers      jsonb       not null default '[]',
    body         text,

    -- held:     waiting on a person.
    -- approved: somebody said yes; the next turn sends it.
    -- declined: somebody said no.
    -- spent:    sent and answered, whatever the answer was.
    -- unsent:   approved, but could not be sent; a person has been asked again.
    -- unknown:  sent, and no answer came; a person has been asked to check.
    -- done:     a person checked and said it needs nothing more.
    state        text        not null default 'held'
        check (state in ('held', 'approved', 'declined', 'spent', 'unsent', 'unknown', 'done')),

    -- Tries that never left. Bounded, after which a person is asked.
    attempts     integer     not null default 0,

    -- The last thing a try reported: the response, or why there was none.
    outcome      jsonb,

    approved_by  uuid                 references users (id) on delete set null,
    approved_at  timestamptz,

    -- When the answer came, which is what spends the grant. Written by the API
    -- when the runtime reports it, since the gateway that checks the grant
    -- holds no database.
    spent_at     timestamptz,

    created_at   timestamptz not null default now(),

    primary key (workspace_id, id),
    foreign key (workspace_id, item_id)
        references action_items (workspace_id, id) on delete cascade
);

create unique index held_requests_item_idx on held_requests (workspace_id, item_id);

-- What a turn reads when it is prepared: the approved requests of its
-- conversation still waiting to go.
create index held_requests_waiting_idx
    on held_requests (workspace_id, session_id)
    where state = 'approved';

-- And what it reads to tell the model what became of the rest.
create index held_requests_session_idx on held_requests (workspace_id, session_id);
