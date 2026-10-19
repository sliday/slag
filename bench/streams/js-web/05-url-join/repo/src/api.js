"use strict";

/**
 * Join an API base URL and a resource path into one URL string.
 */
function buildUrl(base, path) {
  return base + path;
}

module.exports = { buildUrl };
