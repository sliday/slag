"""Product catalog backed by a JSON file."""

import json


class Catalog:
    def __init__(self, items):
        self._items = dict(items)

    @classmethod
    def load(cls, path):
        with open(path, "r", encoding="utf-8") as fh:
            return cls(json.load(fh))

    def lookup(self, sku):
        """Return the item dict for `sku`, or None."""
        return self._items.get(sku)

    def set_price(self, sku, price):
        """Update the price of an existing item."""
        if sku not in self._items:
            raise KeyError(sku)
        if price < 0:
            raise ValueError("price cannot be negative")
        self._items[sku]["price"] = price

    def skus(self):
        """Return all known SKUs in sorted order."""
        return sorted(self._items)
