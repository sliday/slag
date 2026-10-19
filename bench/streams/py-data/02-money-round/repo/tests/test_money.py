import unittest

from pricelib import format_price, price_for, skus


class TestFormatPrice(unittest.TestCase):
    def test_two_decimals_passthrough(self):
        self.assertEqual(format_price("19.99"), "19.99")

    def test_whole_amount_gets_decimals(self):
        self.assertEqual(format_price("5"), "5.00")

    def test_one_decimal_pads(self):
        self.assertEqual(format_price("0.1"), "0.10")

    def test_third_decimal_below_half_rounds_down(self):
        self.assertEqual(format_price("1.234"), "1.23")


class TestCatalog(unittest.TestCase):
    def test_price_for_known_sku(self):
        self.assertEqual(price_for("pen"), "1.25")
        self.assertEqual(price_for("stapler"), "6.49")

    def test_price_for_unknown_sku(self):
        self.assertIsNone(price_for("anvil"))

    def test_skus_sorted(self):
        self.assertEqual(skus(), ["notebook", "pen", "stapler"])


if __name__ == "__main__":
    unittest.main()
