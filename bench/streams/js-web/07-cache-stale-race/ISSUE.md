# Stale data comes back from the dead after an invalidation

Our pricing service sits behind this cache. When a price changes we
invalidate the key and reload. Sometimes the old price comes back
minutes later.

I traced one bad run step by step:

1. A load for the key starts. The upstream is slow, so it is still in
   flight.
2. We call `invalidate(key)` because the price just changed.
3. A second load starts, hits the upstream, and caches the fresh value
   "v2".
4. The first load finally resolves with the old value "v1" and writes
   it into the cache.
5. The next read returns "v1". The fresh price is gone.

Expected: once a key is invalidated, a load that started before the
invalidation must not repopulate the cache. Reads after step 3 keep
returning "v2".

Actual: the slow stale load wins the race and the cache serves "v1"
again.
