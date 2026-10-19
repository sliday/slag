# Records near the cutoff get mis-filtered when timestamps carry offsets

Our feed stores UTC timestamps with a `Z` suffix, and some sources send
numeric offsets like `+02:00`. I filter with `recent_records(records,
cutoff)`, which the README says keeps records strictly newer than the
cutoff.

With cutoff `"2024-03-01T12:00:00Z"` and these records:

    {"id": "before",        "timestamp": "2024-03-01T11:59:59Z"}
    {"id": "at",            "timestamp": "2024-03-01T12:00:00Z"}
    {"id": "after",         "timestamp": "2024-03-01T12:00:01Z"}
    {"id": "offset-after",  "timestamp": "2024-03-01T14:00:01+02:00"}
    {"id": "offset-at",     "timestamp": "2024-03-01T14:00:00+02:00"}
    {"id": "offset-before", "timestamp": "2024-03-01T13:59:59+02:00"}

Expected: only `after` and `offset-after` survive. The `+02:00` forms
are the same instants as the `Z` forms two hours earlier, so
`offset-at` is exactly the cutoff and `offset-before` is one second
before it.

Actual: `offset-at` and `offset-before` also come back. It looks like
the library compares the wall-clock numbers and ignores the offset
entirely. Records with plain naive timestamps filter fine, which is why
our tests never caught this.
