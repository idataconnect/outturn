-- What an operation's file declares about the request it is, parsed at publish
-- so a turn can offer the operation as a tool without reading the file back
-- (docs/skill-operations.md). Null for a file that declares none, which is
-- most of them, and for every file published before this was recorded.
alter table skill_version_files add column operation jsonb;
