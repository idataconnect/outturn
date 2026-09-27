---
approval:
  requires: charge
  covers: booking
  identified_by: booking_id
---

# charge_payment_account

Take money for a booking against a card the house holds.

**Somebody has to approve this before it happens.** You do not ask them and you
do not wait for them in the conversation: make the call as described below, and
if an approval is needed the platform holds the turn, asks whoever may answer,
and runs this turn again once they have. Tell the guest you are putting it
through and that you will confirm — then stop. You will pick up where you left
off.

## The call

`fetch_url` with `POST http://outturn-hollowbrook:8084/charges`, sending JSON.

## What to send

- `payment_account_id` — a handle from `list_payment_accounts`, like `pa_4471`.
  Never a card number; there is no such thing here.
- `booking_id` — the booking this is payment for, from `create_booking` or
  `list_bookings`.
- `amount_pence` — pence, so £145 is `14500`.
- `idempotency_key` — recommended. Any stable string of your own; send the same
  one if you call this again for the same charge, and the house returns the
  first charge rather than taking the money twice.

## What comes back

`201`, with the charge:

- `id`, `payment_account_id`, `booking_id`, `amount_pence`, `created_at`

`200` rather than `201` means a charge with that `idempotency_key` was already
taken and this is that one. Nothing was charged a second time; treat it as
success.

## Errors

- `400` "that is more than the booking's total" — the house will not take more
  than the stay costs. Read the booking's `total_pence` and send that rather
  than a figure you worked out.
- `400` "a charge has to be for something" — `amount_pence` was zero.
- `404` — no such payment account, or no such booking. Check both rather than
  retrying.

## The amount is the booking's, not yours

`create_booking` returns `total_pence` for the whole stay, worked out by the
house. Send that. Multiplying a nightly rate yourself is how a charge ends up a
night out, and the house refuses anything above the total anyway.
