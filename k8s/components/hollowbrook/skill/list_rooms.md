# list_rooms

Every room the house has, whether or not it is free. To find out what is free
for particular dates, use `check_availability` instead.

## The call

`fetch_url` with `GET http://outturn-hollowbrook:8084/rooms`. No parameters.

## What comes back

`200`, with `rooms`, an array of:

- `id` — what a booking names, such as `garden`. Not the room's name.
- `name` — what a person calls it.
- `sleeps` — how many it takes.
- `rate_pence` — one night, in pence.

A room appearing here says nothing about whether it is free.
