#!/bin/sh
# Slag installer — downloads the latest release binary for your platform.
# Usage: curl -sSf https://slag.dev/install.sh | sh

set -e

REPO="sliday/slag"
INSTALL_DIR="$HOME/.slag/bin"

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    darwin) OS="apple-darwin" ;;
    linux)  OS="unknown-linux-gnu" ;;
    *)      echo "Error: Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64)  ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)       echo "Error: Unsupported architecture: $ARCH"; exit 1 ;;
esac

TARGET="${ARCH}-${OS}"

# Get latest release tag
echo "Fetching latest release..."
LATEST=$(curl -sSf "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')

if [ -z "$LATEST" ]; then
    echo "Error: Could not determine latest version"
    exit 1
fi

echo "Installing slag $LATEST for $TARGET..."

# Download
URL="https://github.com/$REPO/releases/download/$LATEST/slag-$TARGET.tar.gz"
TMP=$(mktemp -d)

curl -sSfL "$URL" -o "$TMP/slag.tar.gz" || {
    echo "Error: Download failed. No release for $TARGET?"
    rm -rf "$TMP"
    exit 1
}

# Extract
tar xzf "$TMP/slag.tar.gz" -C "$TMP"

# Install
mkdir -p "$INSTALL_DIR"
mv "$TMP/slag" "$INSTALL_DIR/slag"
chmod +x "$INSTALL_DIR/slag"
rm -rf "$TMP"

echo ""
echo "  Installed slag $LATEST to $INSTALL_DIR/slag"
echo ""

# Check PATH
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo "  Add to your shell profile:"
        echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
        echo ""
        ;;
esac

echo "  Run: slag \"Your Commission\""
echo ""
