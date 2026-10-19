"""Display helpers for name lists."""


def sort_key(name):
    """Return a case-insensitive sort key for `name`."""
    return name.lower()


def initials(name):
    """Return the uppercase initials of a name: 'Ada Lovelace' -> 'AL'."""
    return "".join(part[0].upper() for part in name.split() if part)
