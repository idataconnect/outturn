---
operation:
  method: GET
  url: http://outturn-hollowbrook:8084/bookings
---

# list_bookings

Every booking the house currently holds, oldest first.

## What to send

Nothing, and there is no way to filter — read what comes back and pick out
what you need.

## What comes back

`200`, with `bookings`, an array of:

- `id` — what `get_booking` takes.
- `room_id`, `guest_name`
- `arrival`, `departure` — `YYYY-MM-DD`.
- `total_pence` — the whole stay, in pence.
- `created_at` — when it was made.
