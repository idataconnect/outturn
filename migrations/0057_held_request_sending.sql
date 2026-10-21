-- Held requests in flight ----------------------------------------------------------

-- A held request is marked as going before a runtime sends it. See
-- docs/approvals.md, "The approval carries the call".
--
-- Until this, sending one and reporting what came back were two steps with
-- nothing between them: a pod lost after the first left the request
-- `approved`, and the turn's retry sent it again. A declared write was
-- answered from the idempotency record, but a plain fetch went twice. Marked
-- first, a request found still `sending` by a later turn is one that went, or
-- may have, with no answer -- sent again only where the recipient dedupes on a
-- key, and otherwise put in front of a person, as any other unanswered send.
alter table held_requests drop constraint held_requests_state_check;
alter table held_requests add constraint held_requests_state_check
    check (state in ('held', 'approved', 'sending', 'declined', 'spent', 'unsent', 'unknown',
                     'done'));

-- The turn that marked it, and when. A record of the attempt: what makes a
-- `sending` row stale is that a turn of its conversation is being prepared,
-- which no turn still sending it would allow.
alter table held_requests
    add column sending_job uuid,
    add column sending_at  timestamptz;
