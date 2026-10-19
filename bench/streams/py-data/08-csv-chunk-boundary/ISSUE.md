# Chunked reading corrupts rows with quoted line breaks

I stream large CSV exports with `iter_rows(path, chunk_size=64)` and
rows whose quoted fields contain commas or line breaks come back
shredded. The exact file content, byte for byte (it ends with a newline
after the last line):

```
id,title,body
1,hello,"first line
second line"
2,plain,no quotes
3,"comma, inside",ok
4,last,"multi
line
field"
```

That is a header plus four data records. With `chunk_size=64` I get 8
rows back instead of 5: the comma inside `"comma, inside"` splits that
field in two, and every line break inside quotes starts a bogus extra
row like `['second line"']`.

Expected, at chunk size 64 or any other size:

- 5 rows total
- row 1 is `["1", "hello", "first line\nsecond line"]`
- row 3 is `["3", "comma, inside", "ok"]`
- row 4 is `["4", "last", "multi\nline\nfield"]`

Actual: 8 rows with split fields and garbage rows. Small files with no
quoted fields work fine at any chunk size, which is why the test suite
never caught this.
