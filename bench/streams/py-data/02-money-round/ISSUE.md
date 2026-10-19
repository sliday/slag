# Invoice amounts round the wrong way

We bill some line items at fractional cents and format them with
`format_price` before printing invoices. A customer complained, and
they are right:

- `format_price("2.675")` prints `2.67` on the invoice. It should say
  `2.68`.
- `format_price("0.125")` prints `0.12`. It should say `0.13`.

Rounding money to cents should round half up, the way a cashier does.
Anything at or above half a cent goes up.

Expected: `format_price("2.675")` returns `"2.68"` and
`format_price("0.125")` returns `"0.13"`.

Actual: both round down, so invoices undercharge customers by a cent.

Ordinary amounts like `format_price("19.99")` still print `19.99`, so
this only shows up when the third decimal sits exactly on 5.
