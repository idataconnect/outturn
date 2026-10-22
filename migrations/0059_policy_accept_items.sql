-- What waits on a broad auto-approval policy is said in the queue once per
-- policy: found by the policy it names on every reconcile, and kept to one
-- open item however many reconciles race. The policy is in the item's
-- payload, which nothing else indexes; this covers that lookup and is what
-- refuses the second. See api::auto_approval::waiting.
create unique index action_items_open_policy_accept_idx
    on action_items (workspace_id, (payload->>'policy_id'))
    where kind = 'policy.accept' and state = 'pending';
