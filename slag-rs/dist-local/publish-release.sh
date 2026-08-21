#!/usr/bin/env bash
# Build this host's release assets, then publish them to GitHub Releases.
#
#   ./publish-release.sh --build              refresh the assets, publish nothing
#   ./publish-release.sh "title" "notes"      publish the committed assets
#
# The version comes from slag-rs/Cargo.toml, so a release is cut by bumping
# that file and running this twice: --build, commit, then publish. Nothing
# here is pinned to a version number.
#
# Build and publish stay separate on purpose. `tar` stamps an mtime into the
# archive, so rebuilding always produces different bytes; a publish step that
# rebuilt would upload something other than what was committed and would
# never see a clean tree.
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
TARBALL="slag-$TARGET.tar.gz"
RAW="slag-$TARGET"

if [ -z "$VERSION" ] || [ -z "$TARGET" ]; then
    echo "Error: cannot read version from Cargo.toml or target from rustc" >&2
    exit 1
fi

if [ "${1:-}" = "--build" ]; then
    echo "Building slag $VERSION for $TARGET..."
    (cd "$CRATE_DIR" && cargo build --release)
    BIN="$CRATE_DIR/target/release/slag"
    test -x "$BIN" || { echo "Error: $BIN missing after build" >&2; exit 1; }
    # `slag` is what lives inside the tarball; the target-suffixed copy is
    # the raw asset. Both are the same bytes.
    cp "$BIN" ./slag
    cp "$BIN" "./$RAW"
    tar czf "$TARBALL" slag
    shasum -a 256 "$TARBALL" "$RAW" > sha256.sum
    echo "Assets ready:"
    sed 's/^/  /' sha256.sum
    ./slag --version
    echo "Commit these with the version bump, then publish."
    exit 0
fi

TITLE="${1:-}"
NOTES="${2:-}"
if [ -z "$TITLE" ] || [ -z "$NOTES" ]; then
    echo "Usage: $0 --build | $0 \"title\" \"notes\"" >&2
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

# Publish what is committed, and prove it is the right build first.
for asset in "$TARBALL" "$RAW" sha256.sum; do
    test -f "$asset" || { echo "Error: $asset missing. Run $0 --build." >&2; exit 1; }
done
shasum -a 256 -c sha256.sum >/dev/null || {
    echo "Error: assets do not match sha256.sum. Run $0 --build and commit." >&2
    exit 1
}
BUILT=$(./"$RAW" --version | awk '{print $2}')
if [ "$BUILT" != "$VERSION" ]; then
    echo "Error: asset reports $BUILT but Cargo.toml says $VERSION. Run $0 --build." >&2
    exit 1
fi

echo "Publishing $TAG to $REPO..."
gh release create "$TAG" \
    --repo "$REPO" \
    --title "$TITLE" \
    --notes "$NOTES" \
    "$TARBALL" \
    "$RAW" \
    sha256.sum

echo "Published. Verify with: slag update (should report already up to date)"
