# Page 1 skips the first rows and the last page never shows up

Our user admin list uses this repo to page through accounts. With 10
users and perPage 4 the list is broken in two ways:

1. Page 1 starts at the 5th user. The first 4 users are unreachable;
   every page is shifted forward by one page worth of rows.
2. The response says there are 2 pages total, so the UI never offers
   page 3. The last 2 users (ids 9 and 10) are orphaned.

Concretely, paging 10 users (ids 1 through 10) with perPage 4:

Expected: page 1 returns ids [1, 2, 3, 4], page 2 returns
[5, 6, 7, 8], page 3 returns [9, 10], totalPages is 3, and page 3
reports hasMore false.

Actual: page 1 returns [5, 6, 7, 8], totalPages is 2, and ids 1-4 and
9-10 never appear together anywhere.
