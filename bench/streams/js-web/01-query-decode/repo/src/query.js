"use strict";

/**
 * Parse a query string (no leading "?") into a plain object. Repeated
 * keys collect their values into an array, in order.
 */
function parseQuery(str) {
  const out = {};
  if (!str) return out;
  for (const pair of str.split("&")) {
    const eq = pair.indexOf("=");
    const key = eq === -1 ? pair : pair.slice(0, eq);
    const value = eq === -1 ? "" : pair.slice(eq + 1);
    if (Object.hasOwn(out, key)) {
      if (Array.isArray(out[key])) out[key].push(value);
      else out[key] = [out[key], value];
    } else {
      out[key] = value;
    }
  }
  return out;
}

module.exports = { parseQuery };
