"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { cachedFetch, clearCache } = require("../../src/cache");

test("param order does not matter for the cache key", async () => {
  clearCache();
  let calls = 0;
  const loader = (url) => {
    calls += 1;
    return `payload:${calls}`;
  };
  const first = await cachedFetch(
    "https://api.example.com/report?from=2024-01&to=2024-12",
    loader
  );
  const second = await cachedFetch(
    "https://api.example.com/report?to=2024-12&from=2024-01",
    loader
  );
  assert.equal(calls, 1);
  assert.equal(second, first);
});

test("different param values are different resources", async () => {
  clearCache();
  let calls = 0;
  const loader = () => {
    calls += 1;
    return calls;
  };
  await cachedFetch("https://api.example.com/report?from=2024-01", loader);
  await cachedFetch("https://api.example.com/report?from=2024-02", loader);
  assert.equal(calls, 2);
});

test("identical URLs still share one entry", async () => {
  clearCache();
  let calls = 0;
  const loader = () => {
    calls += 1;
    return "ok";
  };
  await cachedFetch("https://api.example.com/report?a=1&b=2", loader);
  await cachedFetch("https://api.example.com/report?a=1&b=2", loader);
  assert.equal(calls, 1);
});
