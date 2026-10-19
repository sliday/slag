"""Human-readable reconciliation reports."""

from .reconcile import matches, total


def status_line(items, expected):
    """Return a one-line reconciliation status for a sheet."""
    state = "OK" if matches(items, expected) else "MISMATCH"
    return f"{state}: total {total(items)}, expected {expected}"
