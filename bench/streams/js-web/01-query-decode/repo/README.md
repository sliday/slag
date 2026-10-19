# query-toolkit

Query string helpers for the search UI. `parseQuery` turns a query
string (no leading `?`) into a plain object.

## Contract

- `+` means a space, and percent escapes are decoded, per standard
  query string rules. Keys and values are both decoded.
- A repeated key collects its values into an array, in order.

## Tests

    npm test
