# Leaderboard order is wrong

Our weekly newsletter includes a "top players" table generated with
this repo. This week Grace Hopper (200 points) did not show up first.
The table looks like it lists players in reverse signup order, not by
points.

I ran the demo script and see the same thing:

    $ node src/demo.js
      1  AT  Alan Turing (50 pts)
      2  GH  Grace Hopper (200 pts)
      3  AL  Ada Lovelace (120 pts)

Expected: the player with the most points gets rank 1, then descending
by points.

Actual: the order seems unrelated to points; it looks like the input
order flipped around.
