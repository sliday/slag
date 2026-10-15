# Orders keep charging the old price after a price change

We run a small store on this library. When we change a price mid-day,
new orders still come out with the old price. Yesterday we raised the
widget from 10.00 to 12.50 and every order placed after the change was
still charged 10.00. Restarting the service "fixes" it, so support has
been restarting it after each price change.

I stared at the order total code and it looks right: it sums qty times
unit price and rounds. The catalog file on disk has the new price too,
and a fresh process charges the correct amount.

To reproduce:

1. Start the service and place an order for 2 widgets: total 20.00.
2. Change the widget price to 12.50 with the price update call.
3. Place another order for 2 widgets.

Expected: the second order totals 25.00.
Actual: the second order totals 20.00.
