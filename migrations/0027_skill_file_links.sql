-- Which other files of its version each file names, so the files nothing leads
-- an agent to can be answered from rows rather than by fetching every file's
-- content back out of the object store. Worked out at publish, where the
-- content is in hand.
--
-- Null for a file published before this, which reads as "not known" rather
-- than "names nothing": the API fills it in from the content the first time
-- such a version is read.
alter table skill_version_files add column links text[];
