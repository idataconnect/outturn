Hollowbrook House is a guesthouse. You can read its rooms, find what is free
for a stay, and make and look up bookings.

Six operations. Each has a file saying how to call it; read the file for an
operation before using it, with `read_object`. Do not guess a call from its
name here — this list says what exists, not how to ask for it.

- `list_rooms` — every room, with what it sleeps and what it costs a night.
  Detail: `workspace/api/hollowbrook/list_rooms.md`
- `get_room` — the full description of one room: where it is in the house,
  what is in it, what it overlooks, and who it suits.
  Detail: `workspace/api/hollowbrook/get_room.md`
- `check_availability` — which rooms are free for a stay, and what that stay
  would cost. Detail: `workspace/api/hollowbrook/check_availability.md`
- `list_bookings` — every booking currently held.
  Detail: `workspace/api/hollowbrook/list_bookings.md`
- `get_booking` — one booking, by the id given when it was made.
  Detail: `workspace/api/hollowbrook/get_booking.md`
- `create_booking` — reserve a room for a date range.
  Detail: `workspace/api/hollowbrook/create_booking.md`

## The house

Hollowbrook House is on the edge of Hollowbrook village, in the Cotswolds:

    Hollowbrook House, Mill Lane, Hollowbrook, Gloucestershire GL54 2QT
    51.9310, -1.7590

Three rooms, a garden, an orchard. Breakfast is included. Check-in from 3pm
and out by 10am.

Here because they are facts about the house rather than about its API: a guest
asking where it is, or how far from anywhere, is asking something only this
says. Another skill may want the coordinates for its own reasons, and they are
the house's either way.

## Conventions

Two things hold throughout, so they are said once here rather than in every
file. Money is in pence, so £120 is `12000` and nothing is a decimal. Dates
are `YYYY-MM-DD` and name nights: `arrival` is the first night and `departure`
is the morning the guest leaves, so one night means a departure one day later.
