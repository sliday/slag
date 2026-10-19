"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { paginate } = require("../../src/pager");

const ids = Array.from({ length: 10 }, (_, i) => i + 1);

test("page 1 starts at the first item", () => {
  const result = paginate(ids, 1, 4);
  assert.deepEqual(result.items, [1, 2, 3, 4]);
  assert.equal(result.hasMore, true);
});

test("page 2 continues where page 1 stopped", () => {
  const result = paginate(ids, 2, 4);
  assert.deepEqual(result.items, [5, 6, 7, 8]);
  assert.equal(result.hasMore, true);
});

test("page 3 holds the remainder and ends the list", () => {
  const result = paginate(ids, 3, 4);
  assert.deepEqual(result.items, [9, 10]);
  assert.equal(result.hasMore, false);
});

test("totalPages rounds up for a partial last page", () => {
  assert.equal(paginate(ids, 1, 4).totalPages, 3);
});

test("exact multiples still get a full last page", () => {
  const result = paginate([1, 2, 3, 4, 5, 6, 7, 8], 2, 4);
  assert.equal(result.totalPages, 2);
  assert.deepEqual(result.items, [5, 6, 7, 8]);
  assert.equal(result.hasMore, false);
});
