-- A workspace's own bands, and a policy's daily total in each currency. See
-- docs/auto-approval.md, "Risk from the amount".

-- Bands for every operation in one currency, or for one operation -- named as a
-- policy names one, by its skill, its act and its `matches`. The operation's
-- own row wins over its currency's, and either over the operator's defaults.
-- Deleting a row goes back to what stood beneath it. Never past the operator's
-- floor: checked when written, and the floor wins again when a request is
-- assessed, so a row written before a floor was raised cannot reach under it.
create table approval_bands (
    workspace_id uuid        not null references workspaces (id) on delete cascade,
    id           uuid        not null,
    currency     text        not null check (currency ~ '^[A-Z]{3}$'),
    skill_id     uuid,
    requires     text,
    matches      text,
    low          bigint      check (low > 0),
    medium       bigint      check (medium > 0),
    updated_by   uuid                 references users (id) on delete set null,
    updated_at   timestamptz not null default now(),
    primary key (workspace_id, id),
    check ((skill_id is null) = (requires is null) and (skill_id is null) = (matches is null)),
    check (low is null or medium is null or low < medium)
);

create unique index approval_bands_currency_idx
    on approval_bands (workspace_id, currency) where skill_id is null;
create unique index approval_bands_operation_idx
    on approval_bands (workspace_id, currency, skill_id, requires, matches)
    where skill_id is not null;

-- The most of each currency a policy lets out in a day, across everything it
-- covers, in minor units: {"GBP": 50000}. A per-request risk cannot see four
-- hundred charges of £49; this is what bounds an agent splitting one payment.
alter table approval_policies
    add column daily_totals jsonb not null default '{}';

-- An act or operation selector may bound the risk a request works out to as
-- well: "charges, up to low", which is the one line a person writes for the
-- common case. A risk selector still needs one; the others may leave it out.
alter table approval_policies drop constraint approval_policies_check;
alter table approval_policies add constraint approval_policies_check check (
    (selector = 'risk' and max_risk is not null and requires is null and skill_id is null)
    or (selector = 'act' and requires is not null and skill_id is null)
    or (selector = 'operation' and skill_id is not null and matches is not null
        and requires is not null)
);

-- What each total counts: the amount in the operation's declared currency,
-- where the request carried one to trust. Apart from `amount`, which is the
-- summed field a policy names and may be in no currency at all.
alter table auto_approvals
    add column currency        text,
    add column currency_amount bigint;

create index auto_approvals_currency_idx
    on auto_approvals (workspace_id, policy_id, currency, at)
    where currency is not null;
