-- no-transaction
-- `agent_messages.user_id` references users with `on delete set null`, and an
-- unindexed foreign key makes deleting an account scan the busiest table in
-- the system for rows pointing at it. Partial because an assistant's rows,
-- most of them, have none -- the same class as `absorbed_by` in 0024.
--
-- Concurrently, and alone without a transaction, so the build does not hold
-- off every turn writing a reply (see 0061). A build that fails leaves an
-- invalid index under this name and the migration then fails on it; drop it
-- concurrently and start the API again.
create index concurrently agent_messages_user_idx
    on agent_messages (user_id)
    where user_id is not null;
