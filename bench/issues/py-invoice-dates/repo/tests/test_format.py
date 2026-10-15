import unittest
from datetime import date

from billdate.dates import is_leap_year
from billdate.format import human, iso, month_name


class TestFormat(unittest.TestCase):
    def test_iso(self):
        self.assertEqual(iso(date(2026, 3, 15)), "2026-03-15")

    def test_month_name(self):
        self.assertEqual(month_name(date(2026, 3, 15)), "March")
        self.assertEqual(month_name(date(2026, 12, 1)), "December")

    def test_human(self):
        self.assertEqual(human(date(2026, 3, 15)), "15 March 2026")


class TestLeapYear(unittest.TestCase):
    def test_leap(self):
        self.assertTrue(is_leap_year(2024))

    def test_common(self):
        self.assertFalse(is_leap_year(2026))

    def test_century(self):
        self.assertFalse(is_leap_year(2100))
        self.assertTrue(is_leap_year(2000))


if __name__ == "__main__":
    unittest.main()
