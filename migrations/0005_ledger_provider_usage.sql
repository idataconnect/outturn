-- The provider's usage object, verbatim, beside the normalised columns.
--
-- The five token columns are what every provider agrees on and every rate
-- card needs. They are not a superset and never will be: cache writes priced
-- by TTL, service tiers, long-context thresholds, server-side tools billed per
-- call, audio and image tokens -- each provider adds dimensions on its own
-- schedule. Chasing them as columns is a migration per release and a schema
-- still behind.
--
-- So the raw object is kept as it came off the wire. The normalised columns
-- build today's bill; the raw object lets someone re-price yesterday's calls
-- under a dimension nobody thought to normalise, without a backfill, because
-- the data was never dropped.
alter table usage_ledger
    add column provider_usage jsonb,
    -- Normalised on its own because it changes the price of every other
    -- number on the row: OpenAI's flex and priority tiers bill the same
    -- tokens at different rates.
    add column service_tier text;
