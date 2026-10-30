# read_values

The values in one range of a spreadsheet, and the names of its tabs.

## The call

`fetch_url` with:

```
GET https://sheets.googleapis.com/v4/spreadsheets/SPREADSHEET_ID/values/RANGE
```

- `SPREADSHEET_ID` — from the spreadsheet's address.
- `RANGE` — A1 notation with the tab first: `Sheet1!A1:F20`, or `Sheet1!A:F`
  for whole columns. A tab name with spaces or punctuation is quoted and the
  whole range percent-encoded: `'Water bills'!A1:F20` is
  `%27Water%20bills%27!A1:F20`.
- Add `?valueRenderOption=FORMULA` to see formulas rather than what they
  compute.

To find the tabs before choosing a range:

```
GET https://sheets.googleapis.com/v4/spreadsheets/SPREADSHEET_ID?fields=sheets.properties.title
```

## What comes back

`200`, with the rows as arrays of strings, top to bottom:

```
{
  "range": "Sheet1!A1:C3",
  "majorDimension": "ROWS",
  "values": [
    ["Quarter", "Reading", "Amount"],
    ["2026 Q2", "1532", "$214.60"]
  ]
}
```

- Empty trailing cells and rows are left out, so a row may be shorter than
  the range, and `values` is absent for a range that is all empty.
- Values are what the sheet shows, formatted: `$214.60`, not `214.6`.
- `400` names a range it cannot read, usually a tab that does not exist.
  `403` or `404`: the spreadsheet was not shared with the service account, or
  the id is wrong.
