"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { createStore } = require("../src/store");

test("add assigns increasing ids", () => {
  const store = createStore();
  const a = store.add("Ada", "ada@example.com");
  const b = store.add("Grace", "grace@example.com");
  assert.equal(a.id, 1);
  assert.equal(b.id, 2);
});

test("find returns the user or null", () => {
  const store = createStore();
  const a = store.add("Ada", "ada@example.com");
  assert.equal(store.find(a.id).name, "Ada");
  assert.equal(store.find(999), null);
});

test("all returns oldest first and does not leak internals", () => {
  const store = createStore();
  store.add("Ada", "ada@example.com");
  store.add("Grace", "grace@example.com");
  const all = store.all();
  assert.deepEqual(all.map((u) => u.name), ["Ada", "Grace"]);
  all.pop();
  assert.equal(store.count(), 2);
});
