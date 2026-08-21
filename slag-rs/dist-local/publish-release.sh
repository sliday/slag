#!/usr/bin/env bash
# Build this host's release assets and publish them to GitHub Releases.
#
#   ./publish-release.sh --build-only              refresh assets, publish nothing
#   ./publish-release.sh "title" "notes"           build, then publish v<version>
#
# The version comes from slag-rs/Cargo.toml, so a release is cut by bumping
# that file and running this. Nothing here is pinned to a version number.
#
# Assets, matching what install.sh and the self-updater expect:
#   slag-<target>.tar.gz   tarball holding a single `slag` binary
#   slag-<target>          the raw binary, preferred by `slag update`
#   sha256.sum             checksums for both
#
# Prerequisite for publishing: gh auth login -h github.com
set -euo pipefail
cd "$(dirname "$0")"

CRATE_DIR="$(cd .. && pwd)"
REPO="sliday/slag"
VERSION=$(grep -m1 '^version' "$CRATE_DIR/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
TARGET=$(rustc -vV | sed -n 's/^host: //p')
TAG="v$VERSION"

if [ -z "$VERSION" ] || [ -z "$TARGET" ]; then
    echo "Error: cannot read version from Cargo.toml or target from rustc" >&2
    exit 1
fi

echo "Building slag $VERSION for $TARGET..."
(cd "$CRATE_DIR" && cargo build --release)

BIN="$CRATE_DIR/target/release/slag"
test -x "$BIN" || { echo "Error: $BIN missing after build" >&2; exit 1; }

# `slag` is what lives inside the tarball; the target-suffixed copy is the
# raw asset. Both are the same bytes.
cp "$BIN" ./slag
cp "$BIN" "./slag-$TARGET"
tar czf "slag-$TARGET.tar.gz" slag
shasum -a 256 "slag-$TARGET.tar.gz" "slag-$TARGET" > sha256.sum

echo "Assets ready:"
sed 's/^/  /' sha256.sum
./slag --version

if [ "${1:-}" = "--build-only" ]; then
    echo "Build-only: nothing published. Commit these assets with the version bump."
    exit 0
fi

TITLE="${1:-}"
NOTES="${2:-}"
if [ -z "$TITLE" ] || [ -z "$NOTES" ]; then
    echo "Usage: $0 --build-only | $0 \"title\" \"notes\"" >&2
    exit 1
fi

# A published release must be reproducible from a tagged, committed tree.
if [ -n "$(git -C "$CRATE_DIR" status --porcelain)" ]; then
    echo "Error: working tree is dirty. Commit the version bump and assets first." >&2
    exit 1
fi
if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
    echo "Error: release $TAG already exists. Bump the version in Cargo.toml." >&2
    exit 1
fi

echo "Publishing $TAG to $REPO..."
gh release create "$TAG" \
    --repo "$REPO" \
    --title "$TITLE" \
    --notes "$NOTES" \
    "slag-$TARGET.tar.gz" \
    "slag-$TARGET" \
    sha256.sum

echo "Published. Verify with: slag update (should report already up to date)"
