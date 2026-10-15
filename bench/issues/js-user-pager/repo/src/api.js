"use strict";

const { paginate } = require("./paginate");

/**
 * List users for the admin API. `query` carries 1-based `page` and
 * `perPage`. Returns users plus pagination metadata for the response
 * envelope.
 */
function listUsers(store, query = {}) {
  const page = query.page || 1;
  const perPage = query.perPage || 10;
  const result = paginate(store.all(), page, perPage);
  const totalPages = Math.floor(result.total / perPage);
  return {
    users: result.items,
    page,
    perPage,
    total: result.total,
    totalPages,
    hasNext: page < totalPages,
  };
}

module.exports = { listUsers };
