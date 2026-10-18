-- Open questions about a model's upgrade are found by the model they are
-- about, on every tracking pass, approval and delete. The model is in the
-- item's payload, which nothing else indexes; this covers that one lookup,
-- and only the open items of that one kind. See api::tracking.
create index action_items_open_model_upgrade_idx
    on action_items ((payload->>'model_id'))
    where kind = 'model.upgrade' and state = 'pending';
