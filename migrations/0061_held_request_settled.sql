-- When a held request's send was settled ---------------------------------------------

-- Where the model is told what became of an approved request whose refusal
-- could not be found -- trimmed away, or not recognized -- the note goes where
-- it happened: before the first thing said after the send was settled. See
-- docs/approvals.md, "The approval carries the call". Until this it was said
-- only on the turn that sent it, and every later turn was left the stale
-- refusal with nothing to explain it.
alter table held_requests add column settled_at timestamptz;
