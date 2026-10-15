# user-pager

Paginated user listing behind the admin API. Users live in an
in-memory store (oldest signup first); `listUsers` slices out a page.

## Tests

    npm test
