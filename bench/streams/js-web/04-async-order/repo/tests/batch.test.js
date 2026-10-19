"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { fetchAll } = require("../src/batch");

test("loads a single id", async () => {
  const results = await fetchAll(["a"], (id) =>
    Promise.resolve(id.toUpperCase())
  );
  assert.deepEqual(results, ["A"]);
});

test("empty id list resolves to an empty array", async () => {
  const results = await fetchAll([], () => Promise.resolve("x"));
  assert.deepEqual(results, []);
});
