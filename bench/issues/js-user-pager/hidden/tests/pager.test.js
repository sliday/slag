"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { paginate } = require("../../src/paginate");
const { listUsers } = require("../../src/api");
const { createStore } = require("../../src/store");

const numbers = Array.from({ length: 25 }, (_, i) => i + 1);

test("page 1 starts at the first item", () => {
  const result = paginate(numbers, 1, 10);
  assert.deepEqual(result.items, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
});

test("page 2 continues where page 1 stopped", () => {
  const result = paginate(numbers, 2, 10);
  assert.deepEqual(result.items, [11, 12, 13, 14, 15, 16, 17, 18, 19, 20]);
});

test("last page holds the remainder", () => {
  const result = paginate(numbers, 3, 10);
  assert.deepEqual(result.items, [21, 22, 23, 24, 25]);
});

test("totalPages rounds up for partial pages", () => {
  const store = createStore();
  for (let i = 1; i <= 25; i++) store.add(`user${i}`, `u${i}@example.com`);
  const res = listUsers(store, { page: 1, perPage: 10 });
  assert.equal(res.totalPages, 3);
  assert.equal(res.hasNext, true);
});

test("last page reports hasNext false", () => {
  const store = createStore();
  for (let i = 1; i <= 25; i++) store.add(`user${i}`, `u${i}@example.com`);
  const res = listUsers(store, { page: 3, perPage: 10 });
  assert.equal(res.users.length, 5);
  assert.equal(res.hasNext, false);
});

test("exact multiples still get a full last page", () => {
  const store = createStore();
  for (let i = 1; i <= 20; i++) store.add(`user${i}`, `u${i}@example.com`);
  const res = listUsers(store, { page: 2, perPage: 10 });
  assert.equal(res.totalPages, 2);
  assert.equal(res.users.length, 10);
  assert.equal(res.hasNext, false);
});

test("listUsers returns the oldest users on page 1", () => {
  const store = createStore();
  for (let i = 1; i <= 25; i++) store.add(`user${i}`, `u${i}@example.com`);
  const res = listUsers(store, { page: 1, perPage: 10 });
  assert.equal(res.users[0].name, "user1");
  assert.equal(res.users[9].name, "user10");
});
