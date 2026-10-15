"""Price lookups with a small in-memory cache."""


class PriceBook:
    """Resolves SKU prices from a catalog, caching lookups."""

    def __init__(self, catalog):
        self.catalog = catalog
        self._cache = {}

    def price(self, sku):
        """Return the current unit price for `sku`."""
        if sku in self._cache:
            return self._cache[sku]
        item = self.catalog.lookup(sku)
        if item is None:
            raise KeyError(sku)
        self._cache[sku] = item["price"]
        return item["price"]

    def update_price(self, sku, new_price):
        """Change the price of `sku` in the catalog."""
        self.catalog.set_price(sku, new_price)
