import unittest
from datetime import date

from billdate.dates import add_months, next_invoice_date


class TestAddMonths(unittest.TestCase):
    def test_plain_mid_year(self):
        self.assertEqual(add_months(date(2026, 3, 15)), date(2026, 4, 15))

    def test_december_rolls_into_next_year(self):
        self.assertEqual(add_months(date(2026, 12, 15)), date(2027, 1, 15))

    def test_several_months_across_year_end(self):
        self.assertEqual(add_months(date(2026, 11, 30), 3), date(2027, 2, 28))

    def test_january_31_clamps_to_february_28(self):
        self.assertEqual(add_months(date(2026, 1, 31)), date(2026, 2, 28))

    def test_january_31_clamps_to_february_29_in_leap_year(self):
        self.assertEqual(add_months(date(2024, 1, 31)), date(2024, 2, 29))

    def test_october_31_clamps_to_november_30(self):
        self.assertEqual(add_months(date(2026, 10, 31)), date(2026, 11, 30))

    def test_full_year_keeps_day(self):
        self.assertEqual(add_months(date(2026, 6, 15), 12), date(2027, 6, 15))


class TestNextInvoiceDate(unittest.TestCase):
    def test_next_invoice_across_year_end(self):
        self.assertEqual(next_invoice_date(date(2026, 12, 1)), date(2027, 1, 1))


if __name__ == "__main__":
    unittest.main()
