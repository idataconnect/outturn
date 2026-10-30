# sheets

A Google Sheets skill: reading a range, appending rows and updating cells, in
spreadsheets shared with the workspace's own service account.

    scripts/dev.sh --with sheets

The component installs the skill and allows `sheets.googleapis.com`. It does
not hold a key, because the key is the workspace's: see
[docs/service-accounts.md](../../../docs/service-accounts.md) for the design,
and for the traps in the setup below.

## Connecting a spreadsheet

1. In a Google Cloud project -- free, and a personal account can make one --
   enable the Google Sheets API.
2. Create a service account, and a JSON key for it.
3. On the skill's page in outturn, connect a key for `sheets.googleapis.com`:
   paste the whole key file, and give the scope
   `https://www.googleapis.com/auth/spreadsheets`. The page shows the
   account's address.
4. In Google Sheets, share the spreadsheet with that address as Editor.
5. Give the agent the spreadsheet's id, in its instructions or in the
   conversation.

The agent then reaches that spreadsheet and no other, through the Sheets API
alone. Edits show in the spreadsheet's version history under the service
account's address.

## Not yet gated

The writes ask the agent to confirm with the person first, in prose. They
carry no approval rule, because an approval binds top-level scalar fields and
what matters in a Sheets write is the nested `values` array: a gate binding
only the spreadsheet and range would let a second, different row through on
the first one's yes. See docs/approvals.md, *binds*.
