"""Date arithmetic for monthly billing."""

import calendar
from datetime import date


def add_months(d, months=1):
    """Return the date `months` months after `d`.

    When the target month is shorter than the day of `d`, the result
    falls back to the last day of the target month.
    """
    return d.replace(month=d.month + months)


def next_invoice_date(after=None):
    """Return the date of the next monthly invoice run."""
    after = after or date.today()
    return add_months(after, 1)


def is_leap_year(year):
    """Return True when `year` is a leap year."""
    return calendar.isleap(year)
