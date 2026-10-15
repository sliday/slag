# First page of users is missing the oldest accounts

Our admin console calls the user listing from this repo. Two things
look broken:

1. `listUsers(store, { page: 1, perPage: 10 })` never returns the first
   ten signups. The oldest account in the store is "user1" but page 1
   starts at "user11". Page 2 then shows users 21-30, so accounts
   1-10 are unreachable and every page is shifted.
2. With 25 users and perPage 10, the response says `totalPages: 2`, so
   the console never offers page 3 and the last 5 users are orphaned.

Expected: page 1 starts at the oldest user, pages are consecutive with
no gaps, and a partial last page still counts as a page (25 users,
perPage 10 -> 3 pages).

Actual: everything is shifted by one page and the page count drops the
remainder.
