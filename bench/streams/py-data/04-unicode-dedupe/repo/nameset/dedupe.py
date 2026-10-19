"""Deduplicating name lists while keeping first-occurrence order."""


def unique_names(names):
    """Return `names` with duplicates removed.

    The first occurrence of each name wins and the order of first
    appearances is preserved.
    """
    seen = set()
    result = []
    for name in names:
        if name not in seen:
            seen.add(name)
            result.append(name)
    return result
