"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { parseQuery } = require("../../src/query");

test("decodes plus and percent escapes from the issue", () => {
  assert.deepEqual(parseQuery("q=hello+world&city=S%C3%A3o+Paulo"), {
    q: "hello world",
    city: "São Paulo",
  });
});

test("decodes keys as well as values", () => {
  assert.deepEqual(parseQuery("a%20b=c"), { "a b": "c" });
});

test("repeated keys still collect into an array", () => {
  assert.deepEqual(parseQuery("tag=a&tag=b"), { tag: ["a", "b"] });
});

test("repeated keys decode each value", () => {
  assert.deepEqual(parseQuery("tag=a+b&tag=c+d"), {
    tag: ["a b", "c d"],
  });
});
