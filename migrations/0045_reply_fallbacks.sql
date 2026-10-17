-- When a reply was answered by a model other than the one its chain tried
-- first, and why: written by the gateway, the tier that walked the chain and
-- saw the first fail. A row of its own rather than a key in the reply's
-- metadata, which the API writes whole when the turn ends and would race.
-- The first change in a reply is the one kept. See docs/models.md.
create table reply_fallbacks (
    reply_id         uuid        primary key references agent_messages (id) on delete cascade,
    session_id       uuid        not null,
    configured_model text        not null,
    configured_name  text,
    served_model     text        not null,
    served_name      text,
    -- Why the first was passed over, as a clause: "was rate limited".
    reason           text        not null,
    created_at       timestamptz not null default now()
);

create index reply_fallbacks_session_idx on reply_fallbacks (session_id);
