# update_values

Overwrite the cells in one range. For correcting a value that is already
there; to add a row, use `append_values`.

**Show the person what will change — the old values and the new — and wait
for their yes.** What is overwritten is gone except in the spreadsheet's own
version history.

## The call

`fetch_url` with:

```
PUT https://sheets.googleapis.com/v4/spreadsheets/SPREADSHEET_ID/values/RANGE?valueInputOption=USER_ENTERED
Content-Type: application/json

{"values": [["1611", "$231.40"]]}
```

- `RANGE` — exactly the cells being written: `Sheet1!B4:C4`. Read it first,
  so the person can see what it held.
- `values` — rows of cells, the same shape as the range. An empty string `""`
  clears a cell.
- `USER_ENTERED` — as for `append_values`: values read as if typed.

## What comes back

`200`, with `updatedRange` and `updatedCells`. A `400` means the range or a
value could not be read; nothing was written.

Safe to send twice: the same values in the same cells leave the same sheet.
