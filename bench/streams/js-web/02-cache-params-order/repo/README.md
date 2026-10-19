# report-cache

Read-through cache in front of the reporting API. `cachedFetch(url,
loader)` returns the cached response for a URL, calling `loader(url)`
at most once per resource.

## Contract

- The URL identifies the resource. Two URLs that differ only in the
  order of their query parameters name the same resource and share one
  cache entry.
- `loader` may return a value or a promise; results come back as a
  promise either way.

## Tests

    npm test
