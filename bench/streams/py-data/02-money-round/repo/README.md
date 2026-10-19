# pricelib

Price formatting for invoices plus a tiny in-memory catalog.

`format_price(amount)` takes a decimal string and returns it with
exactly two decimals, rounding half up: "2.675" becomes "2.68".

## Tests

    python3 -m unittest discover
