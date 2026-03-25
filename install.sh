#!/bin/sh
# Install rwa — CLI for trading tokenized stocks on Solana
# Usage: curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
#
# Environment variables:
#   INSTALL_DIR   — target directory (default: ~/.cargo/bin)
#   RWA_VERSION   — git ref to install (default: main)
set -e

REPO="outputlayer/rwa_cli"
BIN_NAME="rwa"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.cargo/bin}"
VERSION="${RWA_VERSION:-main}"

# --- Detect OS and architecture ---
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux)   OS="unknown-linux-gnu" ;;
        Darwin)  OS="apple-darwin" ;;
        MINGW*|MSYS*|CYGWIN*) OS="pc-windows-msvc" ;;
        *) echo "Error: unsupported OS: $OS" >&2; exit 1 ;;
    esac

    case "$ARCH" in
        x86_64|amd64)  ARCH="x86_64" ;;
        aarch64|arm64) ARCH="aarch64" ;;
        *) echo "Error: unsupported architecture: $ARCH" >&2; exit 1 ;;
    esac

    PLATFORM="${ARCH}-${OS}"
}

# --- Install Rust if not present ---
ensure_rust() {
    if command -v cargo >/dev/null 2>&1; then
        return 0
    fi

    echo "Rust not found. Installing via rustup..."
    if ! command -v curl >/dev/null 2>&1; then
        echo "Error: 'curl' is required to install Rust" >&2
        exit 1
    fi

    curl -fsSL https://sh.rustup.rs | sh -s -- -y --quiet
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
}

# --- Install from source ---
install_from_source() {
    echo "Installing $BIN_NAME from source (ref: $VERSION)..."
    cargo install --git "https://github.com/${REPO}" --rev "$VERSION" --bin "$BIN_NAME"
}

# --- Main ---
main() {
    echo "Installing $BIN_NAME — trade tokenized stocks on Solana"
    echo ""

    detect_platform
    echo "Platform: $PLATFORM"

    ensure_rust
    install_from_source

    echo ""
    if command -v "$BIN_NAME" >/dev/null 2>&1; then
        echo "Installed: $(command -v $BIN_NAME)"
        echo "Version:   $($BIN_NAME --version 2>/dev/null || echo 'unknown')"
    else
        echo "Installed to: $INSTALL_DIR/$BIN_NAME"
        echo "Make sure $INSTALL_DIR is in your PATH"
    fi

    echo ""
    echo "Quick start:"
    echo "  rwa keys generate     # Create Solana wallet"
    echo "  rwa gm list           # See all 264 tokenized stocks"
    echo "  rwa gm hours          # Check if market is open"
    echo ""
    echo "Agent skills: npx skills add outputlayer/rwa_skills -g"
    echo "Fund your wallet with SOL (gas) + USDC (trading) to start."
}

main "$@"
