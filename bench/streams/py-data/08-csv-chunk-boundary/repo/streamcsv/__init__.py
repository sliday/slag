"""streamcsv: memory-efficient CSV row iteration."""

from .core import iter_rows
from .stats import count_rows

__all__ = ["count_rows", "iter_rows"]
