# Export silently drops rows when a page request fails

Our nightly export uses this repo to walk a paginated endpoint. Last
night the export ran green but contained only a fraction of the rows.

The page sequence was:

    https://api.example.com/items?page=1 -> 200, items ["a", "b"]
    https://api.example.com/items?page=2 -> 500
    https://api.example.com/items?page=3 -> exists, was never fetched

Expected: the export fails loudly. When page 2 returns a 500, the
whole run must fail so the scheduler retries it. A partial export is
worse than no export.

Actual: the export resolved successfully with just the 2 items from
page 1. Pages 2 and 3 vanished without a trace, and the job reported
success.
