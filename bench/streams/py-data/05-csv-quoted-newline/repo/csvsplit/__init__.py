"""csvsplit: split CSV text into rows and fields."""

from .parser import parse
from .report import column, row_count

__all__ = ["column", "parse", "row_count"]
