-- The other half of a trigger's ceiling: how many of its turns may be
-- unfinished at once (docs/triggers.md, "What runs away"). A new session per
-- firing means nothing else joins them, so a hook allowed sixty an hour against
-- turns that take three minutes ran them all at once. Four by default, existing
-- triggers included; null is no limit, said on purpose.
alter table triggers add column max_concurrent int default 4 check (max_concurrent > 0);
update triggers set max_concurrent = 4;
