# API URLs come out with doubled or missing slashes

Our API client builds request URLs with this repo. Depending on how
the caller writes the base and the path, the joined URL is broken.

Two real cases from our logs:

1. base "https://api.example.com/v1/" joined with path "/users"
   produced "https://api.example.com/v1//users". Note the double
   slash. Our gateway 404s on that.
2. base "https://api.example.com/v1" joined with path "users"
   produced "https://api.example.com/v1users". The path glued itself
   to the prefix with no slash at all.

Expected: every combination of those two bases and paths gives
"https://api.example.com/v1/users".

Actual: only base "https://api.example.com/v1/" with path "users"
works. The other three combinations are broken.
