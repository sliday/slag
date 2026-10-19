"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { crawlPages } = require("../../src/crawl");

function makeIssueFetcher() {
  const pages = new Map([
    [null, { items: [1, 2, 3], nextCursor: "c1" }],
    ["c1", { items: [3, 4, 5], nextCursor: "c2" }],
    ["c2", { items: [5, 6, 7], nextCursor: "c2" }],
  ]);
  let calls = 0;
  const fetchPage = (cursor) => {
    calls += 1;
    if (calls > 6) {
      return Promise.reject(new Error("fetchPage called too many times"));
    }
    if (!pages.has(cursor)) {
      return Promise.reject(new Error(`unexpected cursor ${cursor}`));
    }
    return Promise.resolve(pages.get(cursor));
  };
  return { fetchPage, getCalls: () => calls };
}

test("repeated cursor terminates and each item appears once", async () => {
  const { fetchPage } = makeIssueFetcher();
  const items = await crawlPages(fetchPage);
  assert.deepEqual(items, [1, 2, 3, 4, 5, 6, 7]);
});

test("a rejected fetchPage rejects the crawl", async () => {
  const fetchPage = (cursor) => {
    if (cursor === null) {
      return Promise.resolve({ items: [1], nextCursor: "c1" });
    }
    return Promise.reject(new Error("boom"));
  };
  await assert.rejects(crawlPages(fetchPage));
});

test("a clean multi-page crawl returns everything in order", async () => {
  const pages = new Map([
    [null, { items: [1, 2], nextCursor: "c1" }],
    ["c1", { items: [2, 3], nextCursor: null }],
  ]);
  const fetchPage = (cursor) => Promise.resolve(pages.get(cursor));
  assert.deepEqual(await crawlPages(fetchPage), [1, 2, 3]);
});
