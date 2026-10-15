#!/bin/sh
# Runs the hidden tests against the repo passed as $1. Exit 0 means solved.
set -e
REPO=$1
mkdir -p "$REPO/tests"
cp "$REPO"/hidden/tests/*.rs "$REPO/tests/"
cd "$REPO"
exec cargo test --offline
