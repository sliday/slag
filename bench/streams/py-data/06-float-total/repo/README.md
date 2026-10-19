# tallysheet

Reconcile sheets of item prices against expected totals.

`total(items)` sums a list of price strings and returns the total with
exactly two decimals. Sums are rounded half up to cents after summing,
not per item. `matches(items, expected)` reports whether the items sum
to the expected total.

## Tests

    python3 -m unittest discover
