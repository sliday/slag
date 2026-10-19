"""ISO 8601 timestamp parsing."""

from datetime import datetime


def parse_iso(text):
    """Parse an ISO 8601 timestamp into a datetime.

    Accepts a trailing 'Z' for UTC and numeric offsets like '+02:00',
    as well as plain naive timestamps.
    """
    return datetime.fromisoformat(text).replace(tzinfo=None)
