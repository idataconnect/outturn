-- Whether a reply has anything in it, in one place.
--
-- Three things count, and the third is easy to forget: the words, the calls it
-- made, and the points it stopped to think. A reply holding a thought and
-- nothing else is a turn the model spent deliberating -- the only account of
-- where its tokens went -- so treating it as empty discards it, or wedges the
-- session on the abandoned-placeholder guard.
--
-- Written out twice before this, in two statements that had to agree and once
-- did not: the exclusion for a gate refusal was added to one and missed on the
-- other, and the comment beside the survivor claimed a parity that was not
-- there. Both now call this.
--
-- `immutable` so it can still be used in a partial index if one is ever wanted;
-- it reads only its arguments.
create function said_something(content text, metadata jsonb) returns boolean
    immutable
    language sql
as $$
    select content <> ''
        or coalesce(jsonb_array_length(metadata -> 'tool_calls'), 0) > 0
        or exists (
            select 1
            from jsonb_array_elements(coalesce(metadata -> 'parts', '[]'::jsonb)) p
            where p ->> 'type' = 'reasoning'
        )
$$;
