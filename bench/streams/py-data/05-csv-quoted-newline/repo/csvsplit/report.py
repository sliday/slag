"""Small helpers on parsed CSV rows."""


def column(rows, index):
    """Return the values of one column across all rows."""
    return [row[index] for row in rows]


def row_count(rows):
    """Return the number of rows, including the header row."""
    return len(rows)
