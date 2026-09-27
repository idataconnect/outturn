# list_payment_accounts

The cards the house already holds, by handle. Use this to find which payment
account belongs to a guest before charging one.

## The call

`fetch_url` with `GET http://outturn-hollowbrook:8084/payment-accounts`.

## What comes back

`200`, with an array — not wrapped in anything:

- `id` — the handle, like `pa_4471`. This is what `charge_payment_account`
  takes.
- `holder` — whose card it is, as the house records it.
- `label` — what a guest would recognise, like "Visa ending 4471". Say this to
  a guest rather than the handle.

## No card numbers, here or anywhere

A payment account number stands for a card; it is not one. There is no endpoint
that returns a card number and nothing here will ever hand you one, so a guest
asking you to read their card back cannot be helped with — say the label
instead, which is what they will recognise.
