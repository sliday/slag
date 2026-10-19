# Reconciliation flags balanced sheets, and totals drop cents

Our nightly job reconciles sheets of item prices against expected
totals with `matches()` and `total()`. Two problems showed up this
week:

1. A sheet with items `["0.1", "0.2"]` is expected to total `"0.30"`.
   `matches(["0.1", "0.2"], "0.30")` returns False and the report marks
   the sheet MISMATCH, even though a tenth plus two tenths is three
   tenths. The job paged someone over this.

2. A single line item of `"1.005"` totals `"1.00"` instead of `"1.01"`.
   Money should round half up to cents: `total(["1.005"])` should be
   `"1.01"`.

For comparison, these all behave the way I expect today:

- `total(["19.99", "0.01"])` returns `"20.00"`
- `total(["2.675", "0.005"])` returns `"2.68"`

Expected: `matches(["0.1", "0.2"], "0.30")` is True,
`total(["1.005"])` is `"1.01"`, and the totals above keep working.

Actual: balanced sheets get flagged and half-cent amounts round the
wrong way.
