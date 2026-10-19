"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { cachedFetch, clearCache } = require("../src/cache");

test("second fetch of the same URL hits the cache", async () => {
  clearCache();
  let calls = 0;
  const loader = (url) => {
    calls += 1;
    return `data for ${url}`;
  };
  const first = await cachedFetch("https://api.example.com/status", loader);
  const second = await cachedFetch("https://api.example.com/status", loader);
  assert.equal(first, "data for https://api.example.com/status");
  assert.equal(second, first);
  assert.equal(calls, 1);
});

test("different paths load separately", async () => {
  clearCache();
  let calls = 0;
  const loader = () => {
    calls += 1;
    return calls;
  };
  await cachedFetch("https://api.example.com/a", loader);
  await cachedFetch("https://api.example.com/b", loader);
  assert.equal(calls, 2);
});
