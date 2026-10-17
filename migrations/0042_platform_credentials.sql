-- A sealed credential the platform holds rather than a workspace: a provider
-- account's key, for the accounts the operator configures (docs/models.md).
-- What it may reach and who may use it are in its seal, as for any other.
alter table credentials alter column workspace_id drop not null;
