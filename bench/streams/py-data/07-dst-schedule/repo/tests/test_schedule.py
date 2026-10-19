import unittest
from datetime import datetime, timedelta

from schedulite import AppZone, next_run

ZONE = AppZone()


def wall(dt):
    return dt.replace(tzinfo=None)


class TestNextRun(unittest.TestCase):
    def test_next_day_mid_month(self):
        run = next_run(datetime(2024, 5, 14, 10, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 5, 15, 9, 30))

    def test_same_day_before_run_time(self):
        run = next_run(datetime(2024, 5, 14, 9, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 5, 14, 9, 30))

    def test_strictly_after(self):
        run = next_run(datetime(2024, 5, 14, 9, 30, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 5, 15, 9, 30))

    def test_across_month_boundary(self):
        run = next_run(datetime(2024, 6, 30, 12, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 7, 1, 9, 30))


class TestAppZone(unittest.TestCase):
    def test_winter_offset(self):
        self.assertEqual(
            datetime(2024, 1, 15, 12, 0, tzinfo=ZONE).utcoffset(),
            timedelta(hours=-5),
        )

    def test_summer_offset(self):
        self.assertEqual(
            datetime(2024, 7, 15, 12, 0, tzinfo=ZONE).utcoffset(),
            timedelta(hours=-4),
        )

    def test_tzname(self):
        self.assertEqual(datetime(2024, 1, 15, 12, 0, tzinfo=ZONE).tzname(), "EST")
        self.assertEqual(datetime(2024, 7, 15, 12, 0, tzinfo=ZONE).tzname(), "EDT")


if __name__ == "__main__":
    unittest.main()
