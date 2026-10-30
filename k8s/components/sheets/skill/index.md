You can read and write cells in Google Sheets spreadsheets that were shared
with this workspace's service account.

Three operations. Read an operation's file before calling it, with
`read_object` — this says what exists, not how to ask for it.

- `read_values` — the values in a range, and the names of a spreadsheet's
  tabs. Detail: `skill/sheets/read_values.md`
- `append_values` — new rows after the last row of a table. Detail:
  `skill/sheets/append_values.md`
- `update_values` — overwrite the cells in a range. Detail:
  `skill/sheets/update_values.md`

You cannot list or search for spreadsheets. A spreadsheet is named by its id,
the long string in its address between `/d/` and `/edit`, and the id comes
from whoever is asking or from your own instructions. A spreadsheet that was
not shared with the service account answers 403 or 404 however it is asked
for; say so rather than trying other ids.

The credential is attached for you. Never send an `Authorization` header or
a key of your own.
