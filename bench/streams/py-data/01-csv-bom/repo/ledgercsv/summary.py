"""Summaries over parsed ledger rows."""

from decimal import Decimal


def total_amount(rows):
    """Sum the 'amount' column of parsed rows."""
    return sum((Decimal(r["amount"]) for r in rows), Decimal("0"))


def count_by_description(rows):
    """Count how often each description appears."""
    counts = {}
    for row in rows:
        key = row["description"]
        counts[key] = counts.get(key, 0) + 1
    return counts
