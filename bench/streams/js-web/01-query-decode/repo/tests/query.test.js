"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { parseQuery } = require("../src/query");

test("parses simple pairs", () => {
  assert.deepEqual(parseQuery("a=1&b=2"), { a: "1", b: "2" });
});

test("empty string gives an empty object", () => {
  assert.deepEqual(parseQuery(""), {});
});

test("repeated keys collect into an array", () => {
  assert.deepEqual(parseQuery("tag=x&tag=y"), { tag: ["x", "y"] });
});
