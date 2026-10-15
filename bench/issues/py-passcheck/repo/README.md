# passcheck

Tiny password policy helpers: strength checks, 0-4 scoring, salted hashing.

## Usage

    python3 -m passcheck check 'SomePassword1'
    python3 -m passcheck hash 'SomePassword1' 'somesalt'

Policy: at least 8 characters, at least one uppercase letter, at least
one digit.

## Tests

    python3 -m unittest discover
