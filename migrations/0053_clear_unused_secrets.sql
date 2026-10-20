-- A mail hook reached by its address alone checks no secret, so it keeps none:
-- a credential stored and never used is one more thing in every backup.
update triggers set config = jsonb_set(config, '{secret}', '""')
 where kind = 'email' and config->>'scheme' = 'path';
