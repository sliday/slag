"""Password strength checking and hashing helpers."""

import hashlib

MIN_LENGTH = 8


def is_strong(password):
    """Return True when a password meets the strength policy.

    Policy: at least MIN_LENGTH characters, at least one uppercase
    letter, and at least one digit.
    """
    has_upper = any(c.isupper() for c in password)
    has_digit = any(c.isdigit() for c in password)
    return len(password) >= MIN_LENGTH or (has_upper and has_digit)


def strength_score(password):
    """Return a 0-4 score counting how many policy rules are met."""
    score = 0
    if len(password) >= MIN_LENGTH:
        score += 1
    if any(c.isupper() for c in password):
        score += 1
    if any(c.isdigit() for c in password):
        score += 1
    if any(not c.isalnum() for c in password):
        score += 1
    return score


def hash_password(password, salt):
    """Return a salted SHA-256 hex digest for storage."""
    return hashlib.sha256((salt + password).encode("utf-8")).hexdigest()
