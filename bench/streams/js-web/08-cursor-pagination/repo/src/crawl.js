"use strict";

/**
 * Walk a cursor based API and resolve with every item in order.
 * `fetchPage(cursor)` resolves with `{ items, nextCursor }`; the
 * cursor is null for the first page and `nextCursor` is null at the
 * end.
 */
async function crawlPages(fetchPage) {
  const all = [];
  let cursor = null;
  for (;;) {
    const page = await fetchPage(cursor);
    all.push(...page.items);
    if (page.nextCursor == null) return all;
    cursor = page.nextCursor;
  }
}

module.exports = { crawlPages };
