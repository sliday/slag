# streamcsv

Iterate CSV rows without loading the whole file into memory.

`iter_rows(path, chunk_size=4096)` yields rows (lists of field strings,
header first) while reading the file at most `chunk_size` characters at
a time. Fields may be quoted with double quotes, and quoted fields may
contain commas and newlines. The chunk size must not change the result:
any positive chunk size yields the same rows.

## Tests

    python3 -m unittest discover
