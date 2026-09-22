-- Deliveries already accepted, so the same one is not accepted twice.
--
-- A signature is valid for as long as its timestamp is inside the tolerance
-- window, which means a captured request works repeatedly until it ages out.
-- Five minutes is a long time for a request that leaked through a proxy log or
-- a sender's own retry buffer, and each replay is a fresh session and a fresh
-- turn against an agent that may act on the world -- the duplicate-action
-- outcome docs/idempotency.md is about.
--
-- Replay *rejection* rather than idempotency. Idempotency would mean
-- remembering what the first delivery produced and handing it back; this only
-- remembers that it happened. That is all an inbound webhook needs, and it is
-- the difference between a row per delivery for five minutes and a stored
-- result for ever.
create table webhook_deliveries (
    -- The credential presented, hashed rather than kept.
    --
    -- For `hmac` this is the signature, which is already a digest -- but for
    -- `shared_secret` the presented credential *is* the secret, and a table of
    -- secrets in plaintext is worse than the replay it prevents. Hashing both
    -- means this table can be read by anyone who may read the database without
    -- handing them a working credential.
    --
    -- Not a foreign key to the trigger's own secret: what is recorded is what
    -- arrived, so a rotation does not retroactively make old rows meaningless.
    digest      bytea       not null,
    trigger_id  uuid        not null references webhook_triggers (id) on delete cascade,
    -- When this may be forgotten. The signature stops being accepted once its
    -- own timestamp leaves the tolerance window, so remembering it past that
    -- protects nothing and costs rows.
    expires_at  timestamptz not null,
    -- Which request inserted this row.
    --
    -- The row is claimed before the ceiling and before the turn starts, and
    -- released again if neither happens -- so "delete the row for this
    -- digest" is not enough: two identical deliveries racing would have one
    -- release the row the other's refusal is resting on, and the event would
    -- be lost with a 409 already sent saying it had been accepted. The
    -- claimant is what makes a release name its own row and nobody else's.
    claimed_by  uuid        not null,

    -- The whole point: a second delivery presenting the same credential to the
    -- same trigger cannot insert, so it cannot be accepted.
    --
    -- Keyed on the pair rather than the digest alone, because two triggers
    -- with different secrets producing the same digest is not a collision
    -- anybody should have to reason about -- and a global unique index would
    -- let one workspace's traffic refuse another's.
    primary key (trigger_id, digest)
);

-- Supports the sweep, which deletes what can no longer be replayed.
create index webhook_deliveries_expiry_idx on webhook_deliveries (expires_at);
