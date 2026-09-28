-- A prompt's replies, in the order they were attempted ------------------------

-- One reply per prompt was right while the only reason to run a turn twice was
-- a pod dying: the first attempt produced nothing anybody saw, so the retry
-- taking its row back lost nothing, and the unique index made that idempotent.
--
-- An approval breaks that. A turn refused at a gate produces the thing the
-- reader acts on -- the refusal, and the question that follows it -- and then
-- resumes when somebody answers. Taking the row back overwrites exactly the
-- evidence they approved against: the transcript ends up showing a charge that
-- succeeded, with a green "approved" beside it and nothing that ever needed
-- approving. Watched happen; the record was not merely incomplete but wrong.
--
-- So a prompt has an ordered series of attempts, and the constraint moves with
-- it. What the index still guarantees is what it was always for: two turns
-- running concurrently in one session cannot claim the same slot, because they
-- would collide on the same (replies_to, attempt).
alter table agent_messages add column attempt int not null default 1;

drop index agent_messages_replies_to_idx;

create unique index agent_messages_replies_to_attempt_idx
    on agent_messages (replies_to, attempt) where replies_to is not null;

-- Finding a prompt's latest attempt, which is what "the reply" means to
-- everything that asks for one.
create index agent_messages_latest_attempt_idx
    on agent_messages (replies_to, attempt desc) where replies_to is not null;

-- When a message stopped being written, as distinct from when it was created.
--
-- A reply's id is a UUIDv7 assigned when its placeholder is made, at the start
-- of the turn. That is the right thing to order by and the wrong thing to show:
-- a turn that waited thirteen minutes for an approval produced a reply that
-- said "13 minutes ago" the instant it finished streaming.
--
-- Null while a reply is still being written, and on every message that was
-- never streamed into -- a prompt is finished the moment it exists.
alter table agent_messages add column finished_at timestamptz;
