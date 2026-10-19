# Same customer imported twice from two systems

We merge contact names from two CRM systems with `unique_names`, and
some customers show up twice even though the name is identical on
screen.

One concrete case from today's import, written as Python string escapes
so the exact characters are unambiguous:

- system A sends `"caf\u00e9"` (caf plus a single precomposed e-acute
  code point)
- system B sends `"cafe\u0301"` (cafe, then a separate combining acute
  accent code point)

Both spell the same word: cafe with an accented e. On screen they look
identical, but `unique_names(["caf\u00e9", "cafe\u0301"])` returns
both entries instead of one.

Expected: one entry, and it should be the first occurrence's NFC form
`"caf\u00e9"`.

Actual: two entries, so the customer gets mailed twice.

Exact duplicates like `"Ana", "Ana"` dedupe fine. It is only these
two-encoding variants that slip through.
