"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { cachedLoad, invalidate } = require("../../src/ttlcache");

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("a stale in-flight load does not overwrite fresh data", async () => {
  const key = "race-key";
  const slowA = deferred();
  const freshB = deferred();

  const loadA = cachedLoad(key, () => slowA.promise);
  invalidate(key);
  const loadB = cachedLoad(key, () => freshB.promise);

  freshB.resolve("v2");
  assert.equal(await loadB, "v2");

  slowA.resolve("v1");
  await loadA;

  const after = await cachedLoad(key, () => Promise.resolve("v3"));
  assert.equal(after, "v2");
});

test("concurrent loads for one key share a single loader call", async () => {
  const key = "shared-key";
  let calls = 0;
  const gate = deferred();
  const loader = () => {
    calls += 1;
    return gate.promise;
  };
  const first = cachedLoad(key, loader);
  const second = cachedLoad(key, loader);
  gate.resolve("shared");
  assert.equal(await first, "shared");
  assert.equal(await second, "shared");
  assert.equal(calls, 1);
});

test("a load after invalidation returns fresh data", async () => {
  const key = "reload-key";
  let version = 0;
  const loader = () => {
    version += 1;
    return Promise.resolve(`v${version}`);
  };
  assert.equal(await cachedLoad(key, loader), "v1");
  invalidate(key);
  assert.equal(await cachedLoad(key, loader), "v2");
});
