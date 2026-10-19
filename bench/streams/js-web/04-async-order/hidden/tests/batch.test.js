"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { fetchAll } = require("../../src/batch");

const delays = { a: 30, b: 5, c: 10 };

function loader(id) {
  return new Promise((resolve) => {
    setTimeout(() => resolve(id.toUpperCase()), delays[id]);
  });
}

test("results follow the id order, not completion order", async () => {
  const results = await fetchAll(["a", "b", "c"], loader);
  assert.deepEqual(results, ["A", "B", "C"]);
});

test("every id is loaded exactly once", async () => {
  const seen = [];
  const results = await fetchAll(["a", "b", "c"], (id) => {
    seen.push(id);
    return loader(id);
  });
  assert.deepEqual(seen.sort(), ["a", "b", "c"]);
  assert.deepEqual(results, ["A", "B", "C"]);
});

test("empty id list resolves to an empty array", async () => {
  assert.deepEqual(await fetchAll([], loader), []);
});
