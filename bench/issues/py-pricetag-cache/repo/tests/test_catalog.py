import os
import unittest

from shop.catalog import Catalog

DATA = os.path.join(os.path.dirname(__file__), "..", "data", "catalog.json")


class TestCatalog(unittest.TestCase):
    def test_load(self):
        catalog = Catalog.load(DATA)
        self.assertEqual(catalog.lookup("WIDGET")["price"], 10.0)

    def test_lookup_missing_returns_none(self):
        catalog = Catalog.load(DATA)
        self.assertIsNone(catalog.lookup("NOPE"))

    def test_skus_sorted(self):
        catalog = Catalog.load(DATA)
        self.assertEqual(catalog.skus(), ["GADGET", "SPROCKET", "WIDGET"])

    def test_set_price_rejects_unknown_sku(self):
        catalog = Catalog.load(DATA)
        with self.assertRaises(KeyError):
            catalog.set_price("NOPE", 1.0)

    def test_set_price_rejects_negative(self):
        catalog = Catalog.load(DATA)
        with self.assertRaises(ValueError):
            catalog.set_price("WIDGET", -1.0)


if __name__ == "__main__":
    unittest.main()
