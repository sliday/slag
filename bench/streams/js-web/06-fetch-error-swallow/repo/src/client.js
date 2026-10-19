"use strict";

/** Fetch the JSON page at `url` with `fetcher`. */
async function getJson(url, fetcher) {
  try {
    return await fetcher(url);
  } catch {
    return null;
  }
}

/**
 * Walk the page chain starting at `firstUrl` and resolve with every
 * item, in page order. Each page is `{ items, next }`.
 */
async function collectPages(firstUrl, fetcher) {
  const items = [];
  let url = firstUrl;
  while (url) {
    const page = await getJson(url, fetcher);
    if (page === null) break;
    items.push(...page.items);
    url = page.next;
  }
  return items;
}

module.exports = { getJson, collectPages };
