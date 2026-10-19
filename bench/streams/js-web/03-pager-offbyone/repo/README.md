# pager

Offset pagination helper for the list endpoints. `paginate(items,
page, perPage)` slices `items` into a page and returns the slice plus
metadata.

## Contract

- `page` is 1-based: page 1 starts at the first item.
- The result is `{ items, page, perPage, total, totalPages, hasMore }`.
- `totalPages` counts a partial last page as a page.
- `hasMore` is true when a later page exists.

## Tests

    npm test
