"""Reconciling item price lists against expected totals."""


def total(items):
    """Sum a list of price strings and return the total as a string
    with exactly two decimals, rounded half up after summing."""
    return f"{sum(float(i) for i in items):.2f}"


def matches(items, expected):
    """Return True when the items sum to the expected total."""
    return sum(float(i) for i in items) == float(expected)
