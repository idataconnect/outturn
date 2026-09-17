-- What an agent may do with a scope, rather than whether it may write to one.
--
-- `agent_writes_agent_files` and `agent_writes_workspace_files` said whether a
-- write was allowed and left reads alone: every agent could read everything in
-- its space, on the reasoning that one which could not read its own workspace's
-- reference material could not do its job. True of the agent you want reading
-- it, and no help at all for one you don't -- a workspace running a triage
-- agent beside an HR agent had no way to say the first has no business in the
-- shared files, short of a second workspace.
--
-- Three ordered values rather than two flags, because "write but not read" is
-- not a thing anyone means and a pair of booleans invites somebody to configure
-- it and believe it. A write extracts the file's text straight back out, so the
-- host could not honour it even where it was asked for.

-- 'deny' becomes 'read', not 'none': it only ever denied the write, and reads
-- were permitted throughout. Anything else would take away access on upgrade
-- that nobody asked to lose.
update setting_overrides
   set key = 'agent_file_access',
       value = case when value = '"allow"'::jsonb then '"read_write"'::jsonb else '"read"'::jsonb end
 where key = 'agent_writes_agent_files';

update setting_overrides
   set key = 'workspace_file_access',
       value = case when value = '"allow"'::jsonb then '"read_write"'::jsonb else '"read"'::jsonb end
 where key = 'agent_writes_workspace_files';

-- Both renames are conditional on nothing, so a row that somehow held neither
-- name is left where it is rather than guessed at. Any that remain under the
-- old keys are unreadable to the code now and would sit in the table for ever.
delete from setting_overrides
 where key in ('agent_writes_agent_files', 'agent_writes_workspace_files');
