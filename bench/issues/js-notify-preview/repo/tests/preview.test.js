"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { makePreview } = require("../src/preview");
const { buildNotification, PREVIEW_CHARS } = require("../src/notify");

test("short text passes through unchanged", () => {
  assert.equal(makePreview("hello", 20), "hello");
});

test("long ASCII text is cut and gets an ellipsis", () => {
  assert.equal(makePreview("hello world, this is long", 10), "hello worl…");
});

test("text exactly at the limit passes through", () => {
  assert.equal(makePreview("1234567890", 10), "1234567890");
});

test("buildNotification carries title, preview, and body", () => {
  const n = buildNotification("Ada", "your order shipped today");
  assert.equal(n.title, "Message for Ada");
  assert.equal(n.body, "your order shipped today");
  assert.equal(n.preview, "your order shipped t…");
});

test("default limit is used when none is given", () => {
  const n = buildNotification("Ada", "x".repeat(100));
  assert.equal(n.preview.length, PREVIEW_CHARS + 1);
});
