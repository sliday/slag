# Invoice run crashes at month end and year end

We generate invoices monthly with this library. The job has now died
twice in production:

- On December 1st it blew up with `ValueError: month must be in 1..12`.
- On January 31st it blew up with `ValueError: day is out of range for
  month`.

Any invoice scheduled on the 31st seems dangerous, and anything that
crosses into January too. Mid-month runs (say March 15 to April 15)
work fine.

Expected: scheduling from January 31st should land on the last day of
February (28th, or 29th in a leap year), and scheduling in December
should roll over into January of the next year without crashing.

Actual: the run raises ValueError and no invoices go out.
