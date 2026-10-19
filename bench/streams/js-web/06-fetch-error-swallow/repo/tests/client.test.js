"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { collectPages } = require("../src/client");

test("collects items across two pages", async () => {
  const pages = {
    "https://api.example.com/items?page=1": {
      items: ["a", "b"],
      next: "https://api.example.com/items?page=2",
    },
    "https://api.example.com/items?page=2": { items: ["c"], next: null },
  };
  const fetcher = (url) => Promise.resolve(pages[url]);
  const items = await collectPages(
    "https://api.example.com/items?page=1",
    fetcher
  );
  assert.deepEqual(items, ["a", "b", "c"]);
});
