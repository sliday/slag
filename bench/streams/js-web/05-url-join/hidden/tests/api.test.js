"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { buildUrl } = require("../../src/api");

const expected = "https://api.example.com/v1/users";

test("base with trailing slash, path with leading slash", () => {
  assert.equal(buildUrl("https://api.example.com/v1/", "/users"), expected);
});

test("base without trailing slash, path with leading slash", () => {
  assert.equal(buildUrl("https://api.example.com/v1", "/users"), expected);
});

test("base with trailing slash, path without leading slash", () => {
  assert.equal(buildUrl("https://api.example.com/v1/", "users"), expected);
});

test("base without trailing slash, path without leading slash", () => {
  assert.equal(buildUrl("https://api.example.com/v1", "users"), expected);
});
