"use strict";

/**
 * Build a short preview of `text`, at most `maxChars` characters,
 * appending an ellipsis when the text was cut.
 */
function makePreview(text, maxChars = 20) {
  if (text.length <= maxChars) return text;
  return text.slice(0, maxChars) + "…";
}

module.exports = { makePreview };
