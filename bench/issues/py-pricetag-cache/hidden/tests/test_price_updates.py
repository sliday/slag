import unittest

from shop.catalog import Catalog
from shop.orders import Order
from shop.pricing import PriceBook


def make_book():
    catalog = Catalog(
        {
            "WIDGET": {"name": "Standard widget", "price": 10.0},
            "GADGET": {"name": "Deluxe gadget", "price": 24.5},
        }
    )
    return PriceBook(catalog)


class TestPriceAfterUpdate(unittest.TestCase):
    def test_price_reflects_update(self):
        book = make_book()
        self.assertEqual(book.price("WIDGET"), 10.0)
        book.update_price("WIDGET", 12.5)
        self.assertEqual(book.price("WIDGET"), 12.5)

    def test_update_does_not_touch_other_skus(self):
        book = make_book()
        self.assertEqual(book.price("WIDGET"), 10.0)
        self.assertEqual(book.price("GADGET"), 24.5)
        book.update_price("WIDGET", 12.5)
        self.assertEqual(book.price("GADGET"), 24.5)
        self.assertEqual(book.price("WIDGET"), 12.5)

    def test_repeated_updates(self):
        book = make_book()
        book.price("WIDGET")
        book.update_price("WIDGET", 11.0)
        book.update_price("WIDGET", 12.0)
        self.assertEqual(book.price("WIDGET"), 12.0)


class TestOrderTotalsAfterUpdate(unittest.TestCase):
    def test_new_order_uses_new_price(self):
        book = make_book()
        first = Order(book)
        first.add("WIDGET", 2)
        self.assertEqual(first.total(), 20.0)
        book.update_price("WIDGET", 12.5)
        second = Order(book)
        second.add("WIDGET", 2)
        self.assertEqual(second.total(), 25.0)


if __name__ == "__main__":
    unittest.main()
