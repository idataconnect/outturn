-- What a reply was signed with: its thinking blocks and the signatures a
-- provider put on its calls, kept so the next turn can give them back to the
-- model that wrote them. Not on the message: nothing but the model can use
-- them, so the transcript a person reads never carries them, and they can be
-- cleared without touching what was said.
create table reply_signatures (
    message_id  uuid        not null references agent_messages (id) on delete cascade,
    session_id  uuid        not null references agent_sessions (id) on delete cascade,
    -- The reply's first call, which is how the reply is recognized when the
    -- guest replays it.
    first_call  text        not null,
    -- The model that served the reply, as the gateway named it.
    model       text        not null,
    thinking    jsonb       not null default '[]',
    calls       jsonb       not null default '{}',
    created_at  timestamptz not null default now(),
    primary key (message_id, first_call)
);

create index reply_signatures_session_idx on reply_signatures (session_id);
