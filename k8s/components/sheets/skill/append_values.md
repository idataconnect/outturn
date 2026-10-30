# append_values

Add rows after the last row of the table a range is in. The usual way to
record something new without touching what is there.

**Show the person the exact rows first and wait for their yes.** A write lands
in a document other people are reading, and it is not this conversation's to
undo.

## The call

`fetch_url` with:

```
POST https://sheets.googleapis.com/v4/spreadsheets/SPREADSHEET_ID/values/RANGE:append?valueInputOption=USER_ENTERED&insertDataOption=INSERT_ROWS
Content-Type: application/json

{"values": [["2026 Q3", "1611", "=B3-B2", "2026-10-02"]]}
```

- `RANGE` — the table, by its tab and columns: `Sheet1!A:D`. The rows go
  after its last row with anything in it, starting at its first column. Encode
  a quoted tab name as for `read_values`.
- `values` — one array per row, one string per cell, left to right.
- `USER_ENTERED` reads each value as if typed: `=B3-B2` becomes a formula,
  `2026-10-02` a date, `214.60` a number. Formulas use the sheet's own cell
  references, so read the rows above first to know where the new one lands.
- `INSERT_ROWS` adds rows rather than writing over any blank ones below.

## What comes back

`200`, saying where the rows went:

```
{"updates": {"updatedRange": "Sheet1!A4:D4", "updatedRows": 1, "updatedCells": 4}}
```

Tell the person the range. A `400` means the range or a value could not be
read; nothing was written.

Not safe to send twice: a second call adds a second row. If a call's outcome
is unclear, read the table again before deciding.
