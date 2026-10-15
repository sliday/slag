import unittest

from shop.catalog import Catalog
from shop.pricing import PriceBook


def make_book():
    catalog = Catalog({"WIDGET": {"name": "Standard widget", "price": 10.0}})
    return PriceBook(catalog)


class TestPriceBook(unittest.TestCase):
    def test_price_lookup(self):
        self.assertEqual(make_book().price("WIDGET"), 10.0)

    def test_unknown_sku_raises(self):
        with self.assertRaises(KeyError):
            make_book().price("NOPE")


if __name__ == "__main__":
    unittest.main()
