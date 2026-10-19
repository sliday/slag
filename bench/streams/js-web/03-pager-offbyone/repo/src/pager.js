"use strict";

/**
 * Slice `items` into a page. `page` is 1-based. Returns the page items
 * plus pagination metadata.
 */
function paginate(items, page = 1, perPage = 10) {
  const start = page * perPage;
  const totalPages = Math.floor(items.length / perPage);
  return {
    items: items.slice(start, start + perPage),
    page,
    perPage,
    total: items.length,
    totalPages,
    hasMore: page < totalPages,
  };
}

module.exports = { paginate };
