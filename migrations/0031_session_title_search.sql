-- Finding a conversation by its title. Trigram matching, so a partial word
-- finds it: titles are short and typed by people, and "rose" should find
-- "Booking the Rose Room". See docs/session-search.md, step 2.
--
-- pg_trgm ships with Postgres; nothing new to deploy.
create extension if not exists pg_trgm;

create index agent_sessions_title_trgm_idx
    on agent_sessions using gin (title gin_trgm_ops);
