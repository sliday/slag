# Weak passwords are being accepted

I use this little library in the signup flow of a side project. Our
policy is supposed to be: at least 8 characters, at least one uppercase
letter, and at least one digit.

Today I noticed accounts in the database with passwords like `sunshine`
and `qwerty123`. I checked with the command line tool and it agrees
with the database:

    $ python3 -m passcheck check 'sunshine'
    STRONG (score 1/4)

That password has no uppercase letter and no digit, so it should be
rejected. I also tried `Abcdefgh` (no digit) and it passes too.

Expected: a password only counts as strong when it meets every rule.
Actual: passwords that miss rules are accepted as strong.

The scoring and hashing parts seem fine, it is just the yes/no check
that is too lenient.
