-- Models, provider accounts and chains are listed and picked by name, and a
-- credential is shown by its name, so two of one name in one place leave a
-- person choosing between things they cannot tell apart. Duplicates made
-- before this are renamed first: the oldest keeps its name, and the others
-- take a number after it. Ids are UUIDv7, so ordering by id is ordering by
-- age.
--
-- `nulls not distinct`, because a null workspace is the platform's, one
-- place rather than many.

update models m set name = d.name || ' (' || d.n || ')'
from (
    select id, name, row_number() over (partition by workspace_id, name order by id) as n
    from models
) d
where m.id = d.id and d.n > 1;

update provider_accounts a set name = d.name || ' (' || d.n || ')'
from (
    select id, name, row_number() over (partition by workspace_id, name order by id) as n
    from provider_accounts
) d
where a.id = d.id and d.n > 1;

update chains c set name = d.name || ' (' || d.n || ')'
from (
    select id, name, row_number() over (partition by workspace_id, name order by id) as n
    from chains
) d
where c.id = d.id and d.n > 1;

-- Only among credentials still in use: a revoked one keeps its row for the
-- record, and connecting the same host again makes a new one of the same name.
update credentials c set name = d.name || ' (' || d.n || ')'
from (
    select id, name, row_number() over (partition by workspace_id, name order by id) as n
    from credentials
    where revoked_at is null
) d
where c.id = d.id and d.n > 1;

create unique index models_name_unique
    on models (workspace_id, name) nulls not distinct;
create unique index provider_accounts_name_unique
    on provider_accounts (workspace_id, name) nulls not distinct;
create unique index chains_name_unique
    on chains (workspace_id, name) nulls not distinct;
create unique index credentials_name_unique
    on credentials (workspace_id, name) nulls not distinct
    where revoked_at is null;
