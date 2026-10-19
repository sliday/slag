"""A tiny in-memory catalog with formatted prices."""

from .money import format_price

_CATALOG = {
    "pen": "1.25",
    "notebook": "3.80",
    "stapler": "6.49",
}


def price_for(sku):
    """Return the formatted price for `sku`, or None when unknown."""
    price = _CATALOG.get(sku)
    return None if price is None else format_price(price)


def skus():
    """Return the known SKUs in sorted order."""
    return sorted(_CATALOG)
