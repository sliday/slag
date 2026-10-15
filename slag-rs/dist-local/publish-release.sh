#!/usr/bin/env bash
# Build release assets for every installer target, then publish them to GitHub Releases.
#
#   ./publish-release.sh --build              refresh all assets, publish nothing
#   ./publish-release.sh --build --target ... refresh one target, publish nothing
#   ./publish-release.sh --check [--allow-partial] check assets without publishing
#   ./publish-release.sh "title" "notes"      publish the committed assets
#
# The version comes from slag-rs/Cargo.toml, so a release is cut by bumping
# that file and running this twice: --build, then publish. Nothing here is
# pinned to a version number.
#
# Build and publish stay separate on purpose. `tar` stamps an mtime into the
# archive, so rebuilding always produces different bytes; a publish step that
# rebuilt would upload something other than the assets that --build created.
#
# Assets match what install.sh and the self-updater expect:
#   slag-<target>.tar.gz   tarball holding a single `slag` binary
#   slag-<target>          the raw binary, preferred by `slag update`
#   sha256.sum             checksums for both
#
# Linux builds use cargo-zigbuild with a glibc floor of 2.17.
# Prerequisite for publishing: gh auth login -h github.com
set -euo pipefail
cd "$(dirname "$0")"

CRATE_DIR="$(cd .. && pwd)"
REPO="sliday/slag"
VERSION=$(grep -m1 '^version' "$CRATE_DIR/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
TARGET=$(rustc -vV | sed -n 's/^host: //p')
TAG="v$VERSION"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu)
GLIBC_FLOOR="2.17"
CARGO=(rustup run stable cargo)
RUSTC="$(rustup which rustc)"
CARGO_ZIGBUILD_CACHE_DIR="${TMPDIR:-/tmp}/slag-cargo-zigbuild"
ZIG_GLOBAL_CACHE_DIR="${TMPDIR:-/tmp}/slag-zig-cache"

if [ -z "$VERSION" ] || [ -z "$TARGET" ]; then
    echo "Error: cannot read version from Cargo.toml or target from rustc" >&2
    exit 1
fi

verify_binary() {
    local raw="$1"
    local target="$2"
    local details
    details=$(file "$raw")
    echo "  $details"
    case "$target" in
        aarch64-*) echo "$details" | grep -Eqi 'arm64|aarch64' || { echo "Error: $raw has the wrong architecture" >&2; exit 1; } ;;
        x86_64-*) echo "$details" | grep -Eqi 'x86_64|x86-64' || { echo "Error: $raw has the wrong architecture" >&2; exit 1; } ;;
    esac
}

require_rust_target() {
    local target="$1"
    rustup target list --installed | grep -qx "$target" || {
        echo "Error: Rust target $target is missing. Install it with: rustup target add $target" >&2
        exit 1
    }
}

build_target() {
    local target="$1"
    local bin
    local raw="slag-$target"
    local tarball="$raw.tar.gz"
    local build_target="$target"

    case "$target" in
        aarch64-apple-darwin)
            (cd "$CRATE_DIR" && RUSTC="$RUSTC" "${CARGO[@]}" build --release)
            bin="$CRATE_DIR/target/release/slag"
            ;;
        x86_64-apple-darwin)
            require_rust_target "$target"
            (cd "$CRATE_DIR" && RUSTC="$RUSTC" "${CARGO[@]}" build --release --target "$target")
            bin="$CRATE_DIR/target/$target/release/slag"
            ;;
        x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
            require_rust_target "$target"
            command -v cargo-zigbuild >/dev/null 2>&1 || {
                echo "Error: cargo-zigbuild is missing. Install it with: cargo install cargo-zigbuild" >&2
                exit 1
            }
            command -v zig >/dev/null 2>&1 || {
                echo "Error: zig is missing. Install it with: brew install zig" >&2
                exit 1
            }
            build_target="$target.$GLIBC_FLOOR"
            (cd "$CRATE_DIR" && RUSTC="$RUSTC" CARGO_ZIGBUILD_CACHE_DIR="$CARGO_ZIGBUILD_CACHE_DIR" ZIG_GLOBAL_CACHE_DIR="$ZIG_GLOBAL_CACHE_DIR" "${CARGO[@]}" zigbuild --release --target "$build_target")
            bin="$CRATE_DIR/target/$target/release/slag"
            if [ ! -x "$bin" ]; then
                bin="$CRATE_DIR/target/$build_target/release/slag"
            fi
            ;;
        *)
            echo "Error: unsupported target $target. Choose one of: ${TARGETS[*]}" >&2
            exit 1
            ;;
    esac

    test -x "$bin" || {
        echo "Error: $bin missing after building $target" >&2
        exit 1
    }
    cp "$bin" "./$raw"
    local stage
    stage=$(mktemp -d)
    cp "$bin" "$stage/slag"
    tar czf "$tarball" -C "$stage" slag
    rm -rf "$stage"
    verify_binary "$raw" "$target"
}

if [ "${1:-}" = "--build" ]; then
    selected_targets=("${TARGETS[@]}")
    if [ "${2:-}" = "--target" ]; then
        [ -n "${3:-}" ] || { echo "Usage: $0 --build [--target <triple>]" >&2; exit 1; }
        selected_targets=("$3")
    elif [ -n "${2:-}" ]; then
        echo "Usage: $0 --build [--target <triple>]" >&2
        exit 1
    fi

    echo "Building slag $VERSION with glibc floor $GLIBC_FLOOR for: ${selected_targets[*]}"
    for target in "${selected_targets[@]}"; do
        build_target "$target"
    done
    if [ ! -x "slag-$TARGET" ]; then
        build_target "$TARGET"
    fi
    cp "slag-$TARGET" ./slag
    assets=()
    for asset in slag-*; do
        [ -f "$asset" ] && assets+=("$asset")
    done
    shasum -a 256 "${assets[@]}" | sort -k2 > sha256.sum
    if [[ " ${selected_targets[*]} " == *" $TARGET "* ]]; then
        host_raw="slag-$TARGET"
        host_version=$(./"$host_raw" --version | awk '{print $2}')
        [ "$host_version" = "$VERSION" ] || {
            echo "Error: host asset reports $host_version but Cargo.toml says $VERSION" >&2
            exit 1
        }
        echo "  $host_raw --version: $host_version"
    fi
    if docker info >/dev/null 2>&1; then
        for target in "${selected_targets[@]}"; do
            case "$target" in
                x86_64-unknown-linux-gnu) platform=linux/amd64 ;;
                aarch64-unknown-linux-gnu) platform=linux/arm64 ;;
                *) continue ;;
            esac
            echo "  Docker $platform $target:"
            docker run --rm --platform "$platform" -v "$PWD:/release:ro" debian:stable-slim "/release/slag-$target" --version
        done
    else
        echo "  Docker unavailable, skipped Linux runtime checks. Start Docker and rerun --build to enable them."
    fi
    echo "Assets ready:"
    sed 's/^/  /' sha256.sum
    echo "Keep these assets and run the publish command when ready."
    exit 0
fi

CHECK=false
ALLOW_PARTIAL=false
args=()
for arg in "$@"; do
    case "$arg" in
        --check) CHECK=true ;;
        --allow-partial) ALLOW_PARTIAL=true ;;
        *) args+=("$arg") ;;
    esac
done
TITLE="${args[0]:-}"
NOTES="${args[1]:-}"
if ! "$CHECK" && { [ -z "$TITLE" ] || [ -z "$NOTES" ]; }; then
    echo "Usage: $0 --check [--allow-partial] | $0 [--allow-partial] \"title\" \"notes\"" >&2
    exit 1
fi

# Publish the generated assets, and prove they match the checksum file first.
test -f sha256.sum || { echo "Error: sha256.sum missing. Run $0 --build." >&2; exit 1; }
assets=()
while read -r checksum asset; do
    if [ ! -f "$asset" ] || ! printf '%s  %s\n' "$checksum" "$asset" | shasum -a 256 -c - >/dev/null 2>&1; then
        if "$ALLOW_PARTIAL"; then
            echo "Skipping missing or invalid asset: $asset" >&2
            continue
        fi
        echo "Error: $asset is missing or fails its checksum. Run $0 --build." >&2
        exit 1
    fi
    assets+=("$asset")
done < sha256.sum
if ! "$ALLOW_PARTIAL"; then
    for target in "${TARGETS[@]}"; do
        for required in "slag-$target" "slag-$target.tar.gz"; do
            found=false
            for asset in "${assets[@]}"; do
                [ "$asset" != "$required" ] || found=true
            done
            "$found" || { echo "Error: sha256.sum lacks $required. Run $0 --build or use --allow-partial." >&2; exit 1; }
        done
    done
fi
[ "${#assets[@]}" -gt 0 ] || { echo "Error: no valid release assets." >&2; exit 1; }
if "$CHECK"; then
    echo "Release asset check passed: ${#assets[@]} assets."
    exit 0
fi

# A published release must use the current script and version.
DIRTY=$(git -C "$CRATE_DIR" status --porcelain | grep -vE '^[ MARCUD?]{2} dist-local/(sha256\.sum|slag|slag-[^/]+)$' || true)
if [ -n "$DIRTY" ]; then
    echo "Error: working tree has changes outside generated release assets." >&2
    exit 1
fi
BUILT=$(./"slag-$TARGET" --version | awk '{print $2}')
if [ "$BUILT" != "$VERSION" ]; then
    echo "Error: asset reports $BUILT but Cargo.toml says $VERSION. Run $0 --build." >&2
    exit 1
fi

echo "Publishing $TAG to $REPO..."
if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
    echo "Error: release $TAG already exists. Bump the version in Cargo.toml." >&2
    exit 1
fi
gh release create "$TAG" \
    --repo "$REPO" \
    --title "$TITLE" \
    --notes "$NOTES" \
    "${assets[@]}" sha256.sum

echo "Published. Verify with: slag update (should report already up to date)"
