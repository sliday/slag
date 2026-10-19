"""Row counting helpers built on iter_rows."""

from .core import iter_rows


def count_rows(path, chunk_size=4096):
    """Return the number of rows in a CSV file, header included."""
    return sum(1 for _ in iter_rows(path, chunk_size))
