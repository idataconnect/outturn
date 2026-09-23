# create_booking

Reserve a room for a date range. The stay's total is worked out and returned,
so you are not asked to calculate it.

## The call

`fetch_url` with `POST http://outturn-hollowbrook:8084/bookings`, sending JSON.

## What to send

All four are required.

- `room_id` — a room's id, from `list_rooms` or the `room_id` of a vacancy in
  `check_availability`. Not the room's name.
- `guest_name`
- `arrival` — `YYYY-MM-DD`, the first night.
- `departure` — `YYYY-MM-DD`, the morning they leave. Must be after `arrival`;
  a same-day departure is refused rather than read as one night.

## What comes back

`201`, with the booking:

- `id` — quote this to the guest, and keep it: `get_booking` takes it.
- `total_pence` — the whole stay, in pence.
- `room_id`, `guest_name`, `arrival`, `departure`, `created_at`

## Errors

- `409` with `{"error": "..."}` — the room is taken for part of that range.
  Call `check_availability` for the same dates and offer what is free rather
  than trying again with the same room.
- `400` — the dates are malformed, or `departure` is not after `arrival`.
- `404` — no room has that id. Check it against `list_rooms`.
