"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { cachedLoad, invalidate } = require("../src/ttlcache");

test("second load of the same key hits the cache", async () => {
  let calls = 0;
  const loader = () => {
    calls += 1;
    return Promise.resolve("v1");
  };
  assert.equal(await cachedLoad("basic-hit", loader), "v1");
  assert.equal(await cachedLoad("basic-hit", loader), "v1");
  assert.equal(calls, 1);
});

test("invalidate forces a reload", async () => {
  let version = 0;
  const loader = () => {
    version += 1;
    return Promise.resolve(`v${version}`);
  };
  assert.equal(await cachedLoad("basic-invalidate", loader), "v1");
  invalidate("basic-invalidate");
  assert.equal(await cachedLoad("basic-invalidate", loader), "v2");
});
