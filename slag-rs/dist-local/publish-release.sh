#!/usr/bin/env bash
# One-shot v2.0.0 GitHub release. Prerequisite: gh auth login -h github.com
set -euo pipefail
cd "$(dirname "$0")"

gh release create v2.0.0 \
  --repo sliday/slag \
  --title "slag 2.0.0 — native forge engine" \
  --notes "slag no longer shells out to claude/codex/opencode. Native OpenRouter agentic engine (one API key), sandboxed tools with fuzzy edit ladder, Ratatui dashboard with live steering (--tui), twin-cast duels judged by an independent model, recipes system, semver-guarded self-updater with tarball extraction. 174 tests." \
  slag-aarch64-apple-darwin.tar.gz \
  slag-aarch64-apple-darwin \
  sha256.sum

echo "Release published. Verify: slag update (should say already up to date)"
