# ttlcache

Read-through cache in front of the pricing service. `cachedLoad(key,
loader)` returns the cached value for `key`, calling `loader()` when
the value is missing. `invalidate(key)` drops the cached value.

## Contract

- Concurrent loads for the same key share one loader call.
- `invalidate(key)` supersedes any load already in flight for that
  key: when the superseded load resolves late, it must not put its
  stale value back into the cache.
- The next load after an invalidation sees fresh data.

## Tests

    npm test
