# export-client

Paged JSON client behind the nightly data export. `collectPages(
firstUrl, fetcher)` walks a page chain and returns every item.

## Contract

- `fetcher(url)` returns a promise for one page: `{ items, next }`,
  where `next` is the URL of the next page or null at the end.
- `collectPages` resolves with the concatenation of all page items, in
  page order.
- A failed request must reject the export. Silently dropping the
  remaining pages corrupts the export.

## Tests

    npm test
