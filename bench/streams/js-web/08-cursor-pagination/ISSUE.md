# Sync job hangs forever and duplicates rows when the API repeats a cursor

Our sync job crawls a cursor based endpoint with this repo. Against
one tenant the job never finishes, and the rows it has written so far
contain duplicates.

Here is the exact page sequence the API returns:

    fetchPage(null)  -> { items: [1, 2, 3], nextCursor: "c1" }
    fetchPage("c1")  -> { items: [3, 4, 5], nextCursor: "c2" }
    fetchPage("c2")  -> { items: [5, 6, 7], nextCursor: "c2" }

Two things go wrong:

1. The API is inclusive of the cursor item: page 2 starts at 3, which
   was also the last item of page 1, and page 3 starts at 5, the last
   item of page 2. Our rows come out as [1, 2, 3, 3, 4, 5, 5, ...]
   with 3 and 5 written twice.
2. Page 3 hands back the cursor "c2" again, the same cursor we just
   used. The crawler asks for "c2" forever and the job never exits.

Expected: the crawl terminates and resolves with exactly
[1, 2, 3, 4, 5, 6, 7], each item once.

Actual: it loops on "c2" until we kill the job.

A single page feed (nextCursor null on the first page) works fine.
