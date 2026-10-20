-- An operator's word on what makes one of an operation's writes the same as
-- another, from which the host derives an idempotency key
-- (docs/idempotency.md). Proposed by the wizard, never applied by it: a wrong
-- identity merges writes somebody meant to be two.
alter table skill_annotations drop constraint skill_annotations_kind_check;
alter table skill_annotations add constraint skill_annotations_kind_check
    check (kind in ('note', 'prefer', 'hidden', 'approval', 'identity'));
