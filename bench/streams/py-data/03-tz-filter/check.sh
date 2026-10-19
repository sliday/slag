#!/bin/sh
# Runs the hidden tests against the repo passed as $1. Exit 0 means solved.
set -e
cd "$1"
exec python3 -m unittest discover -s hidden -t . -v
