-- A setting may be chosen for one model: Gemini's temperature, Sonnet's
-- effort. Nil, as agent_id is, for a value that holds for every model -- every
-- row there is today -- so nothing existing changes meaning. See
-- docs/models.md, "Settings belong to a model".
alter table setting_overrides
    add column model_id uuid not null default '00000000-0000-0000-0000-000000000000';

alter table setting_overrides drop constraint setting_overrides_pkey;
alter table setting_overrides add primary key (workspace_id, agent_id, model_id, key);

-- A model's rows go with the model. A plain foreign key cannot express "nil
-- or a real model", so a trigger does, as for agents.
create function drop_model_setting_overrides() returns trigger as $$
begin
    delete from setting_overrides where model_id = old.id;
    return old;
end;
$$ language plpgsql;

create trigger model_setting_overrides_go_with_the_model
    after delete on models
    for each row execute function drop_model_setting_overrides();
