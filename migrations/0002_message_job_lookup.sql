-- The transcript reports, per user message, the state of the job answering
-- it -- so a reader can be told "queued", "failed" or nothing rather than
-- guessing from an empty reply. That is a lookup by the message id inside the
-- payload, which without this is a scan of every turn ever queued.
create index jobs_chat_turn_message_idx
    on jobs (((payload->>'message_id')::uuid))
    where kind = 'chat.turn';
