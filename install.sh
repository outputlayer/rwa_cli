#!/bin/sh
# Install rwa — CLI for trading tokenized stocks on Solana
# Usage: curl -fsSL https://raw.githubusercontent.com/user/rwa_cli/main/install.sh | sh
set -e

REPO="user/rwa_cli"
BIN_NAME="rwa"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.cargo/bin}"

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

# --- Check for required tools ---
check_deps() {
    for cmd in curl tar; do
        if ! command -v "$cmd" >/dev/null 2>&1; then
            echo "Error: '$cmd' is required but not found" >&2
            exit 1
        fi
    done
}

# --- Install from source (requires Rust) ---
install_from_source() {
    if command -v cargo >/dev/null 2>&1; then
        echo "Installing $BIN_NAME from source..."
        cargo install --git "https://github.com/${REPO}" --bin "$BIN_NAME"
        echo ""
        echo "Installed: $(which $BIN_NAME)"
        echo "Version:   $($BIN_NAME --version)"
        return 0
    fi
    return 1
}

# --- Install Rust if not present ---
install_rust() {
    if ! command -v cargo >/dev/null 2>&1; then
        echo "Rust not found. Installing via rustup..."
        curl -fsSL https://sh.rustup.rs | sh -s -- -y --quiet
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    fi
}

# --- Main ---
main() {
    echo "Installing $BIN_NAME — trade tokenized stocks on Solana"
    echo ""

    detect_platform
    echo "Platform: $PLATFORM"

    # Try source install (most reliable for any platform)
    install_rust
    install_from_source

    echo ""
    echo "Setup:"
    echo "  rwa keys generate     # Create Solana wallet"
    echo "  rwa gm list           # See all 264 tokenized stocks"
    echo "  rwa gm hours          # Check if market is open"
    echo ""
    echo "Fund your wallet with SOL (gas) + USDC (trading) to start."
}

main "$@"
