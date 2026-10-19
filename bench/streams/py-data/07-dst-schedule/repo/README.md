# schedulite

A daily scheduler with its own small DST-aware time zone, `AppZone`
(US-style rules for 2024: spring forward 2024-03-10 02:00, fall back
2024-11-03 02:00, offsets UTC-5 standard and UTC-4 daylight).

`next_run(after)` returns the next daily run strictly after the aware
datetime `after`. Runs fire at 09:30 local wall time every local day,
including DST transition days. The result is aware and carries the
correct offset for its date: UTC-4 from 2024-03-10 through 2024-11-02,
UTC-5 otherwise.

## Tests

    python3 -m unittest discover
