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

# Add to PATH if needed
case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        echo "  PATH already configured."
        ;;
    *)
        # Detect shell and profile file
        SHELL_NAME=$(basename "${SHELL:-/bin/sh}")
        case "$SHELL_NAME" in
            zsh)  PROFILE="$HOME/.zshrc" ;;
            bash)
                # macOS uses .bash_profile, Linux uses .bashrc
                if [ -f "$HOME/.bash_profile" ]; then
                    PROFILE="$HOME/.bash_profile"
                else
                    PROFILE="$HOME/.bashrc"
                fi
                ;;
            fish) PROFILE="$HOME/.config/fish/config.fish" ;;
            *)    PROFILE="$HOME/.profile" ;;
        esac

        EXPORT_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""

        # Check if already in profile
        if [ -f "$PROFILE" ] && grep -qF "$INSTALL_DIR" "$PROFILE" 2>/dev/null; then
            echo "  PATH already in $PROFILE"
        else
            # Append to profile
            echo "" >> "$PROFILE"
            echo "# slag" >> "$PROFILE"
            echo "$EXPORT_LINE" >> "$PROFILE"
            echo "  Added to $PROFILE"
            echo ""
            echo "  Activate now with:"
            echo "    source $PROFILE && slag --version"
        fi
        ;;
esac

echo ""
echo "  Run: slag \"Your Commission\""
echo ""
