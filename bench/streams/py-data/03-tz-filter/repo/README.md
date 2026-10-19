# feedsift

Filter feed records by their ISO 8601 timestamps.

`recent_records(records, cutoff)` returns the records whose `timestamp`
field is strictly newer than `cutoff`, keeping input order. A record
exactly at the cutoff does not count as recent.

Timestamps may carry an offset: `Z` for UTC or a numeric offset like
`+02:00`. Offsets are honored, so `2024-03-01T14:00:01+02:00` is the
same instant as `2024-03-01T12:00:01Z`. Timestamps with no offset are
treated as UTC.

## Tests

    python3 -m unittest discover
