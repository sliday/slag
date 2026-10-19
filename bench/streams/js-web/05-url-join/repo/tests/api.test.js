"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { buildUrl } = require("../src/api");

test("joins a base with a trailing slash and a plain path", () => {
  assert.equal(
    buildUrl("https://api.example.com/v1/", "users"),
    "https://api.example.com/v1/users"
  );
});
