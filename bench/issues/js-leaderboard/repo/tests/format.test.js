"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { initials, formatRow } = require("../src/format");

test("initials takes the first letter of each word", () => {
  assert.equal(initials("Ada Lovelace"), "AL");
  assert.equal(initials("grace hopper"), "GH");
  assert.equal(initials("Cher"), "C");
});

test("formatRow pads the rank and shows points", () => {
  assert.equal(
    formatRow({ rank: 1, name: "Ada Lovelace", points: 120 }),
    "  1  AL  Ada Lovelace (120 pts)"
  );
});

test("formatRow tolerates a missing rank", () => {
  assert.equal(
    formatRow({ name: "Cher", points: 5 }),
    "  -  C  Cher (5 pts)"
  );
});
