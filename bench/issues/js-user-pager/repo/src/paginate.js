"use strict";

/**
 * Slice `items` into pages. `page` is 1-based. Returns the page items
 * plus pagination metadata.
 */
function paginate(items, page = 1, perPage = 10) {
  const start = page * perPage;
  return {
    items: items.slice(start, start + perPage),
    page,
    perPage,
    total: items.length,
  };
}

module.exports = { paginate };
