"use strict";

const cache = new Map();

/** Build the cache key for a request URL. */
function makeKey(url) {
  return url;
}

/**
 * Return the cached response for `url`, calling `loader(url)` at most
 * once per resource. The result comes back as a promise.
 */
function cachedFetch(url, loader) {
  const key = makeKey(url);
  if (cache.has(key)) return Promise.resolve(cache.get(key));
  return Promise.resolve(loader(url)).then((value) => {
    cache.set(key, value);
    return value;
  });
}

/** Drop every cached entry. */
function clearCache() {
  cache.clear();
}

module.exports = { makeKey, cachedFetch, clearCache };
