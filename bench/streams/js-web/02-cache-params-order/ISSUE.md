# Same report fetched twice when params come in a different order

Our dashboard uses this repo to cache report requests. The analytics
show the same report pulled twice in a row whenever two widgets build
their URLs in a different order.

Concretely, the dashboard requested both of these:

    https://api.example.com/report?from=2024-01&to=2024-12
    https://api.example.com/report?to=2024-12&from=2024-01

Expected: one network fetch. Both URLs ask for the same report, the
parameters are just listed in a different order, so the second request
should hit the cache.

Actual: two network fetches. The cache treats the two URLs as
different resources.

Fetching the exact same URL twice does hit the cache, so this only
shows up when the parameter order differs.
