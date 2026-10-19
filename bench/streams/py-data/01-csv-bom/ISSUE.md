# Accounting export crashes on the date column

My accounting tool exports a monthly ledger as CSV and I load it with
`read_rows` from this library. The export starts with an invisible
UTF-8 BOM character: the first three bytes of the file are `EF BB BF`,
and right after them comes this exact text:

```
date,description,amount
2024-03-04,Office supplies,42.50
2024-03-05,Client lunch,18.20
```

(The file ends with a newline after the last row.)

When I read this file, `rows[0]["date"]` raises `KeyError: 'date'`.
Printing the keys of the first row shows the culprit: the first header
comes out as `'\ufeffdate'` instead of `'date'`.

Expected: `rows[0]["date"]` returns `"2024-03-04"`, and both rows parse
with the keys `date`, `description`, `amount`.

Actual: the BOM glues itself to the first header name and every lookup
by `date` fails.

Files I save myself without the BOM work fine.
