"""Filtering feed records by timestamp."""

from .timestamps import parse_iso


def recent_records(records, cutoff):
    """Return the records strictly newer than `cutoff`, in input order.

    `records` is an iterable of dicts with an ISO 8601 'timestamp'
    field. `cutoff` is an ISO 8601 string. A record counts as recent
    only when its timestamp is strictly newer than the cutoff.
    """
    cut = parse_iso(cutoff)
    return [r for r in records if parse_iso(r["timestamp"]) > cut]
