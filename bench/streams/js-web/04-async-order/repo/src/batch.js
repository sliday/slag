"use strict";

/**
 * Run `loader(id)` for every id and resolve with the results in the
 * same order as `ids`.
 */
function fetchAll(ids, loader) {
  return new Promise((resolve, reject) => {
    const results = [];
    let remaining = ids.length;
    if (remaining === 0) {
      resolve(results);
      return;
    }
    ids.forEach((id) => {
      Promise.resolve(loader(id)).then((value) => {
        results.push(value);
        remaining -= 1;
        if (remaining === 0) resolve(results);
      }, reject);
    });
  });
}

module.exports = { fetchAll };
