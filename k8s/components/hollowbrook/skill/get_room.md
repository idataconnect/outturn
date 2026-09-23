# get_room

What one room is actually like — where it is in the house, what is in it, what
it overlooks, and anything about it worth knowing before booking.

`list_rooms` does not carry this. It has what you need to tell rooms apart —
the name, what it sleeps, what it costs — and nothing more, so listing three
rooms does not mean reading three descriptions.

## The call

`fetch_url` with `GET http://outturn-hollowbrook:8084/rooms/{id}`, putting the
room's id in the path.

## What comes back

`200`, with the room:

- `id`, `name`, `sleeps`, `rate_pence` — as in `list_rooms`.
- `description` — a paragraph about the room.

## Errors

- `404` with `{"error": "..."}` — no room has that id. Check it against
  `list_rooms`; a room's *name* is not its id.

## When to call it

When somebody asks what a room is like, or why they might choose one over
another. Once, for the room they asked about.

Not to build a list. Someone asking what rooms there are wants
`list_rooms` — fetching all three descriptions to answer that is three calls
and a wall of text where four lines would do.
