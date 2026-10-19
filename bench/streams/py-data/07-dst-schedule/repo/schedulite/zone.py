"""A small deterministic tzinfo with US-style DST rules for 2024.

Standard time is UTC-5 (EST), daylight time is UTC-4 (EDT).
Daylight time starts on 2024-03-10 at 02:00 local standard time (the
clock jumps to 03:00) and ends on 2024-11-03 at 02:00 local daylight
time (the clock falls back to 01:00). Outside 2024 the rules are not
defined; the zone is meant for the 2024 calendar.
"""

from datetime import datetime, timedelta, tzinfo

STD_OFFSET = timedelta(hours=-5)
DST_OFFSET = timedelta(hours=-4)

SPRING_FORWARD = datetime(2024, 3, 10, 2, 0)
FALL_BACK = datetime(2024, 11, 3, 2, 0)


class AppZone(tzinfo):
    """US-style DST rules for 2024, with no system tzdata dependency."""

    def _is_dst(self, dt):
        wall = dt.replace(tzinfo=None)
        if SPRING_FORWARD <= wall < FALL_BACK:
            # The hour 01:00-02:00 on fall-back day happens twice; the
            # second occurrence (fold=1) is standard time.
            if wall.date() == FALL_BACK.date() and dt.fold == 1:
                return False
            return True
        return False

    def utcoffset(self, dt):
        if dt is None:
            return STD_OFFSET
        return DST_OFFSET if self._is_dst(dt) else STD_OFFSET

    def dst(self, dt):
        if dt is None:
            return timedelta(0)
        return timedelta(hours=1) if self._is_dst(dt) else timedelta(0)

    def tzname(self, dt):
        if dt is None:
            return "EST"
        return "EDT" if self._is_dst(dt) else "EST"
