import unittest

from pricelib import format_price


class TestFormatPriceHalfUp(unittest.TestCase):
    def test_issue_values_round_half_up(self):
        # Exact values quoted in ISSUE.md.
        self.assertEqual(format_price("2.675"), "2.68")
        self.assertEqual(format_price("0.125"), "0.13")

    def test_more_half_cases(self):
        self.assertEqual(format_price("1.005"), "1.01")
        self.assertEqual(format_price("10.555"), "10.56")

    def test_ordinary_values(self):
        self.assertEqual(format_price("19.99"), "19.99")
        self.assertEqual(format_price("5"), "5.00")
        self.assertEqual(format_price("0.1"), "0.10")
        self.assertEqual(format_price("0.10"), "0.10")

    def test_below_half_rounds_down(self):
        self.assertEqual(format_price("2.674"), "2.67")
        self.assertEqual(format_price("0.124"), "0.12")


if __name__ == "__main__":
    unittest.main()
