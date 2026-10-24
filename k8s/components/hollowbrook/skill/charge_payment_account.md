---
approval:
  requires: charge
  matches: POST /charges
  binds: [payment_account_id, booking_id, amount_pence]
  covers: booking
  identified_by: booking_id
  risk: high
  numbers: [amount_pence]
  amount: { field: amount_pence, currency: GBP, minor_units: 2 }
  bands: { low: 5000, medium: 20000 }
  free: [idempotency_key]
operation:
  method: POST
  url: http://outturn-hollowbrook:8084/charges
  body: [payment_account_id, booking_id, amount_pence]
  integer: [amount_pence]
  identity: [booking_id, amount_pence]
  key: field idempotency_key
---

# charge_payment_account

Take money for a booking against a card the house holds.

**Somebody may have to approve this before it happens.** You do not ask them
and you do not wait for them in the conversation: call it once. If an approval
is needed the platform holds the turn and asks whoever may answer; once they
say yes, the platform sends the charge itself and you are given what came
back. Do not send it again. Tell the guest you are putting it through and that
you will confirm — then stop. You will pick up where you left off.

## What to send

- `payment_account_id` — a handle from `list_payment_accounts`, like `pa_4471`.
  Never a card number; there is no such thing here.
- `booking_id` — the booking this is payment for, from `create_booking` or
  `list_bookings`.
- `amount_pence` — pence, so £145 is `14500`.

A charge for the same booking and amount is the same charge, however many
times it is sent. The platform tells the house so; there is nothing to add for
it.

## What comes back

`201`, with the charge:

- `id`, `payment_account_id`, `booking_id`, `amount_pence`, `created_at`

`200` rather than `201` means that charge was already taken and this is that
one. Nothing was charged a second time; treat it as success.

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
