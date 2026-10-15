"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { makePreview } = require("../../src/preview");
const { buildNotification } = require("../../src/notify");

function hasLoneSurrogate(s) {
  for (let i = 0; i < s.length; i++) {
    const code = s.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = s.charCodeAt(i + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return true;
      i++;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return true;
    }
  }
  return false;
}

test("previews never contain a broken emoji", () => {
  const preview = makePreview("Hi 🙂 there friend", 4);
  assert.equal(hasLoneSurrogate(preview), false);
});

test("the limit counts characters, not UTF-16 units", () => {
  const preview = makePreview("🙂".repeat(25), 20);
  const chars = Array.from(preview);
  assert.equal(chars.length, 21);
  assert.equal(chars[chars.length - 1], "…");
  assert.equal(chars.slice(0, 20).join(""), "🙂".repeat(20));
});

test("emoji at the boundary is kept whole or dropped, never halved", () => {
  const preview = makePreview("call me 📞 when free", 9);
  assert.equal(hasLoneSurrogate(preview), false);
  assert.ok(Array.from(preview).length <= 10);
});

test("short emoji text under the limit passes through unchanged", () => {
  assert.equal(makePreview("hi 🙂", 20), "hi 🙂");
});

test("notification preview survives emoji-heavy bodies", () => {
  const n = buildNotification("Ada", "🎉 shipped! " + "🎉".repeat(30));
  assert.equal(hasLoneSurrogate(n.preview), false);
});
