# Search filters show raw escapes instead of real text

Our search page reads its filters from the URL with this repo. A user
shared a search link and the filter panel rendered garbage.

The URL query string was:

    q=hello+world&city=S%C3%A3o+Paulo

Expected: the panel shows q as "hello world" and city as "São Paulo".

Actual: the panel shows q as "hello+world" and city as
"S%C3%A3o+Paulo". The plus signs and percent escapes come through
verbatim.

Simple filters like `a=1&b=2` work fine, so this only shows up once a
search has a space or a non-ASCII character in it.
