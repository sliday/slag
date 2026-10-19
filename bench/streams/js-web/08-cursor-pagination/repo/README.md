# crawler

Cursor pagination crawler behind the sync job. `crawlPages(fetchPage)`
walks a cursor based API and resolves with every item in order.

## Contract

- `fetchPage(cursor)` resolves with `{ items, nextCursor }`. `cursor`
  is null for the first page. `nextCursor` is null on the last page.
- The API is inclusive of the cursor item: the first item of a page
  repeats the last item of the previous page. The result must contain
  each item once.
- Some APIs return a cursor that was already seen. The crawl must
  terminate instead of looping.
- A rejected fetchPage rejects the crawl.

## Tests

    npm test
