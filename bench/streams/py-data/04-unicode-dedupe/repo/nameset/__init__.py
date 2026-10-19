"""nameset: dedupe name lists from multiple import sources."""

from .dedupe import unique_names
from .display import initials, sort_key

__all__ = ["initials", "sort_key", "unique_names"]
