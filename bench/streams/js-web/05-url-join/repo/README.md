# api-client

URL building for the internal API client. `buildUrl(base, path)` joins
an API base URL and a resource path into one URL string.

## Contract

- The result always has exactly one slash where base and path meet,
  whether or not the base ends with `/` and the path starts with `/`.
- Any path prefix on the base is preserved:
  `buildUrl("https://api.example.com/v1", "users")` keeps the `/v1`
  prefix.

## Tests

    npm test
