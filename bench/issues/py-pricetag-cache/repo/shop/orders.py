"""Orders: line items priced through a PriceBook."""


class Order:
    def __init__(self, price_book):
        self.price_book = price_book
        self.lines = []

    def add(self, sku, qty=1):
        """Add `qty` units of `sku` at the current price."""
        if qty <= 0:
            raise ValueError("qty must be positive")
        unit = self.price_book.price(sku)
        self.lines.append({"sku": sku, "qty": qty, "unit": unit})

    def total(self):
        """Return the order total rounded to cents."""
        return round(sum(line["qty"] * line["unit"] for line in self.lines), 2)
