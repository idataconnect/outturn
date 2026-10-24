-- What an operation declares about money: which bound field is the amount, in
-- what currency, with default bands and a floor. See docs/auto-approval.md,
-- "Risk from the amount". Beside the gate it qualifies, written at publish from
-- the same frontmatter block, as `egress::amount::Declaration`. Null for an
-- operation that moves none, whose risk is its base risk whatever it carries.
alter table skill_version_gates
    add column amount jsonb;
