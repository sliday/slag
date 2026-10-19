"""Reading CSV ledger exports into dicts."""

import csv


def read_rows(path):
    """Read a CSV ledger export and return a list of row dicts.

    The first line of the file is the header row. Every row maps a
    header name to the field text.
    """
    with open(path, newline="") as f:
        return list(csv.DictReader(f))
