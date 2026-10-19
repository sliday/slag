"""Price formatting for the catalog and invoices."""


def format_price(amount):
    """Format a decimal string amount with exactly two decimals.

    Rounds half up, like a cashier would: '2.675' becomes '2.68' and
    '0.125' becomes '0.13'.
    """
    return f"{round(float(amount), 2):.2f}"
