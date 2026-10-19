"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { collectPages } = require("../../src/client");

const PAGE1 = "https://api.example.com/items?page=1";
const PAGE2 = "https://api.example.com/items?page=2";
const PAGE3 = "https://api.example.com/items?page=3";

function makeFetcher(failPage2) {
  return (url) => {
    if (url === PAGE1) {
      return Promise.resolve({ items: ["a", "b"], next: PAGE2 });
    }
    if (url === PAGE2) {
      if (failPage2) return Promise.reject(new Error("HTTP 500"));
      return Promise.resolve({ items: ["c", "d"], next: PAGE3 });
    }
    if (url === PAGE3) {
      return Promise.resolve({ items: ["e"], next: null });
    }
    return Promise.reject(new Error(`unexpected url ${url}`));
  };
}

test("a failing page rejects the export instead of truncating it", async () => {
  await assert.rejects(collectPages(PAGE1, makeFetcher(true)));
});

test("a healthy chain resolves with every item", async () => {
  const items = await collectPages(PAGE1, makeFetcher(false));
  assert.deepEqual(items, ["a", "b", "c", "d", "e"]);
});
