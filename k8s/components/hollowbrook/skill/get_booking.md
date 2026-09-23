# get_booking

One booking, by the id it was given when it was made.

## The call

`fetch_url` with `GET http://outturn-hollowbrook:8084/bookings/{id}`, putting
the id in the path.

## What comes back

`200`, with the booking itself — not wrapped in anything:

- `id`, `room_id`, `guest_name`
- `arrival`, `departure` — `YYYY-MM-DD`.
- `total_pence` — the whole stay, in pence.
- `created_at`

## Errors

- `404` with `{"error": "..."}` — no booking has that id. Check the id rather
  than retrying; it will not appear later.
