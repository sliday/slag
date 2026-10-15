"""Command line interface for passcheck."""

import sys

from .core import hash_password, is_strong, strength_score


def main(argv=None):
    argv = argv if argv is not None else sys.argv[1:]
    if len(argv) < 2:
        print("usage: python3 -m passcheck check <password>")
        print("       python3 -m passcheck hash <password> <salt>")
        return 2
    command = argv[0]
    if command == "check":
        password = argv[1]
        verdict = "STRONG" if is_strong(password) else "WEAK"
        print(f"{verdict} (score {strength_score(password)}/4)")
        return 0
    if command == "hash":
        if len(argv) < 3:
            print("hash needs a salt")
            return 2
        print(hash_password(argv[1], argv[2]))
        return 0
    print(f"unknown command: {command}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
