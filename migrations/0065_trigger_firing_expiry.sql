-- How long a firing is remembered ---------------------------------------------------

-- A firing is recorded so the same delivery sent twice runs once, and senders
-- retry for hours, not months. Kept thirty days from firing and then swept by
-- the API's tick (`trigger::forget_old_firings`); until this nothing deleted
-- a row, so the table held every firing a trigger ever had. A redelivery
-- after that is taken as new, which no sender's retry schedule reaches.
--
-- Backfilled from each row's own firing rather than from today, so the first
-- sweep after this lands takes what is already past it.
alter table trigger_firings add column expires_at timestamptz;
update trigger_firings set expires_at = created_at + interval '30 days';
alter table trigger_firings
    alter column expires_at set not null,
    alter column expires_at set default now() + interval '30 days';

create index trigger_firings_expiry_idx on trigger_firings (expires_at);
