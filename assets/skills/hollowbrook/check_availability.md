# check_availability

Which rooms are free for a stay, and what the stay would cost. The total is
worked out for you, so you do not have to multiply a rate by the nights.

## The call

`fetch_url` with
`GET http://outturn-hollowbrook:8084/availability?arrival=YYYY-MM-DD&departure=YYYY-MM-DD`.

Both are required. `departure` must be after `arrival`.

## What comes back

`200`, with:

- `arrival`, `departure` — as asked.
- `nights` — what the range works out to.
- `available` — an array, one per free room:
  - `room_id` — **`room_id` here**, not `id` as in `list_rooms`. This is what
    `create_booking` takes.
  - `name`, `sleeps`, `rate_pence`
  - `total_pence` — the whole stay.

An empty `available` means nothing is free for those dates. That is an answer,
not a failure: say so rather than trying other dates unless you were asked to.

## Errors

- `400` with `{"error": "..."}` — `departure` is not after `arrival`.
