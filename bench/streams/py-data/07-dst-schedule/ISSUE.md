# Daily job fires an hour late after spring forward, an hour early after fall back

Our daily report job uses `next_run()` and is supposed to fire at 09:30
local time every day. The README promises 09:30 local wall time even on
DST transition days. Twice a year it goes wrong:

- Calling `next_run()` any time after the 09:30 run on 2024-03-09 (say
  at 10:00 that morning) returns a run at 10:30 on 2024-03-10, the
  spring-forward day. It should be 09:30 on 2024-03-10.
- Same story in the fall: after the 09:30 run on 2024-11-02, the next
  run comes back as 08:30 on 2024-11-03 instead of 09:30.

The offsets on the returned values look right otherwise: a run on
2024-03-10 should carry the daylight offset (UTC-4) and a run on
2024-11-03 should carry the standard offset (UTC-5). It is the wall
clock time that jumps.

Expected:

- `next_run(datetime(2024, 3, 9, 10, 0, tzinfo=AppZone()))` -> 2024-03-10 09:30 local
- `next_run(datetime(2024, 11, 2, 10, 0, tzinfo=AppZone()))` -> 2024-11-03 09:30 local

Actual: 10:30 on 2024-03-10 and 08:30 on 2024-11-03. Mid-month dates
like 2024-05-14 behave fine, which is why the test suite never caught
this.
