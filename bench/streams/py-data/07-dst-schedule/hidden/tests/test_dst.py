import unittest
from datetime import datetime, timedelta

from schedulite import AppZone, next_run

ZONE = AppZone()


def wall(dt):
    return dt.replace(tzinfo=None)


class TestNextRunAcrossDST(unittest.TestCase):
    def test_spring_forward_day_stays_at_0930(self):
        # Exact call quoted in ISSUE.md.
        run = next_run(datetime(2024, 3, 9, 10, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 3, 10, 9, 30))
        self.assertEqual(run.utcoffset(), timedelta(hours=-4))

    def test_day_after_spring_forward(self):
        run = next_run(datetime(2024, 3, 10, 10, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 3, 11, 9, 30))
        self.assertEqual(run.utcoffset(), timedelta(hours=-4))

    def test_fall_back_day_stays_at_0930(self):
        # Exact call quoted in ISSUE.md.
        run = next_run(datetime(2024, 11, 2, 10, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 11, 3, 9, 30))
        self.assertEqual(run.utcoffset(), timedelta(hours=-5))

    def test_day_after_fall_back(self):
        run = next_run(datetime(2024, 11, 3, 10, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 11, 4, 9, 30))
        self.assertEqual(run.utcoffset(), timedelta(hours=-5))

    def test_consecutive_runs_across_transition(self):
        first = next_run(datetime(2024, 3, 8, 10, 0, tzinfo=ZONE))
        self.assertEqual(wall(first), datetime(2024, 3, 9, 9, 30))
        second = next_run(first)
        self.assertEqual(wall(second), datetime(2024, 3, 10, 9, 30))
        third = next_run(second)
        self.assertEqual(wall(third), datetime(2024, 3, 11, 9, 30))

    def test_mid_month_unaffected(self):
        run = next_run(datetime(2024, 5, 14, 10, 0, tzinfo=ZONE))
        self.assertEqual(wall(run), datetime(2024, 5, 15, 9, 30))


if __name__ == "__main__":
    unittest.main()
