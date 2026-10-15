"""Formatting helpers for billing dates."""

MONTH_NAMES = [
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
]


def month_name(d):
    """Return the English name of the month of `d`."""
    return MONTH_NAMES[d.month - 1]


def iso(d):
    """Return the ISO 8601 form of `d`."""
    return d.isoformat()


def human(d):
    """Return `d` as '15 March 2026'."""
    return f"{d.day} {month_name(d)} {d.year}"
