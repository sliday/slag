"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { paginate } = require("../src/pager");

test("empty input stays empty", () => {
  const result = paginate([], 1, 10);
  assert.deepEqual(result.items, []);
  assert.equal(result.total, 0);
  assert.equal(result.totalPages, 0);
  assert.equal(result.hasMore, false);
});

test("echoes page, perPage, and total", () => {
  const result = paginate([1, 2, 3], 1, 2);
  assert.equal(result.page, 1);
  assert.equal(result.perPage, 2);
  assert.equal(result.total, 3);
});
