-- Amounts kept for a whole currency are in that currency's standard minor
-- units, by ISO 4217 (`egress::amount::standard_minor_units`): pence for GBP,
-- yen for JPY, fils for KWD. Before, a currency's bands were applied in each
-- operation's own units, so one row meant £50 to an operation counting pence
-- and £5,000 to one counting whole pounds; and a day's total summed whatever
-- each operation counted in. See docs/auto-approval.md, "As built".
--
-- Nothing is converted. A currency's row holds one figure for operations that
-- may count in different units, so there is no single factor to convert it
-- by; and for an operation counting in its currency's standard units -- pence
-- for pounds, as every example has -- the old meaning and the new are one.
-- An operation's own row stays in its own units, as before.

comment on column approval_bands.low is
    'Upper bound for low. For a currency''s row (skill_id null), in the currency''s '
    'standard minor units; for an operation''s, in its amount field''s own.';
comment on column approval_bands.medium is
    'Upper bound for medium, in the units approval_bands.low is in.';
comment on column approval_policies.daily_totals is
    'The most of each currency let out in a day, in its standard minor units: {"GBP": 50000}.';
comment on column auto_approvals.currency_amount is
    'The amount in its currency''s standard minor units, rounded up from finer ones.';
