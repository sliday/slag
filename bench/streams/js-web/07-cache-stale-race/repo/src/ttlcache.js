"use strict";

const cache = new Map();

/**
 * Return the cached value for `key`, calling `loader()` when the value
 * is missing. The result comes back as a promise.
 */
function cachedLoad(key, loader) {
  if (cache.has(key)) return Promise.resolve(cache.get(key));
  return Promise.resolve()
    .then(() => loader())
    .then((value) => {
      cache.set(key, value);
      return value;
    });
}

/** Drop the cached value for `key`. */
function invalidate(key) {
  cache.delete(key);
}

module.exports = { cachedLoad, invalidate };
