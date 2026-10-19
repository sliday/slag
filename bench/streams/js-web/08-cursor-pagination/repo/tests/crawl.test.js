"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { crawlPages } = require("../src/crawl");

test("a single page crawl returns its items", async () => {
  const fetchPage = (cursor) => {
    assert.equal(cursor, null);
    return Promise.resolve({ items: [1, 2], nextCursor: null });
  };
  assert.deepEqual(await crawlPages(fetchPage), [1, 2]);
});
