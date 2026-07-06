#!/bin/sh
# Install rwa — CLI for trading tokenized stocks on Solana
# Usage: curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
#
# Environment variables:
#   INSTALL_DIR   — target directory (default: ~/.cargo/bin)
#   RWA_VERSION   — release tag to install (default: latest). Use "main" to build from source.
set -e

REPO="outputlayer/rwa_cli"
BIN_NAME="rwa"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.cargo/bin}"
VERSION="${RWA_VERSION:-latest}"

PLATFORM=""
ARCHIVE_EXT=""
BIN_PATH_IN_ARCHIVE=""

need_cmd() {
    command -v "$1" >/dev/null 2>&1
}

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
    if [ "$OS" = "pc-windows-msvc" ]; then
        ARCHIVE_EXT="zip"
        BIN_PATH_IN_ARCHIVE="${BIN_NAME}.exe"
    else
        ARCHIVE_EXT="tar.gz"
        BIN_PATH_IN_ARCHIVE="${BIN_NAME}"
    fi
}

release_base_url() {
    case "$VERSION" in
        latest)
            printf '%s\n' "https://github.com/${REPO}/releases/latest/download"
            ;;
        main|master)
            return 1
            ;;
        *)
            printf '%s\n' "https://github.com/${REPO}/releases/download/${VERSION}"
            ;;
    esac
}

download() {
    url="$1"
    output="$2"
    if need_cmd curl; then
        curl -fsSL "$url" -o "$output"
    elif need_cmd wget; then
        wget -qO "$output" "$url"
    else
        echo "Error: need curl or wget to download release binaries" >&2
        return 1
    fi
}

verify_checksum() {
    archive="$1"
    checksums="$2"
    archive_name="$(basename "$archive")"
    checksum_file="$(dirname "$archive")/SHA256SUMS.single"

    if ! grep " ${archive_name}\$" "$checksums" > "$checksum_file"; then
        echo "Warning: no checksum entry found for ${archive_name} — skipping verification" >&2
        return 0
    fi

    if need_cmd sha256sum; then
        (
            cd "$(dirname "$archive")"
            sha256sum -c "$(basename "$checksum_file")"
        )
    elif need_cmd shasum; then
        (
            cd "$(dirname "$archive")"
            shasum -a 256 -c "$(basename "$checksum_file")"
        )
    else
        if [ "${RWA_INSTALL_INSECURE:-}" != "1" ]; then
            echo "ERROR: cannot verify download (sha256sum/shasum not found)." >&2
            echo "Fix: install coreutils (sha256sum) — or build from source:" >&2
            echo "  cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa" >&2
            echo "Or bypass verification at your own risk: RWA_INSTALL_INSECURE=1 <installer>" >&2
            exit 1
        fi
        echo "WARNING: RWA_INSTALL_INSECURE=1 set — installing WITHOUT checksum verification." >&2
    fi
}

extract_archive() {
    archive="$1"
    dest="$2"

    case "$ARCHIVE_EXT" in
        tar.gz)
            tar -xzf "$archive" -C "$dest"
            ;;
        zip)
            if need_cmd unzip; then
                unzip -q "$archive" -d "$dest"
            elif need_cmd powershell.exe; then
                powershell.exe -NoProfile -Command "Expand-Archive -Path '$archive' -DestinationPath '$dest' -Force" >/dev/null
            else
                echo "Error: need unzip or powershell.exe to extract Windows archive" >&2
                return 1
            fi
            ;;
        *)
            echo "Error: unsupported archive format: $ARCHIVE_EXT" >&2
            return 1
            ;;
    esac
}

install_prebuilt() {
    base_url="$(release_base_url)" || return 1
    archive="rwa-${PLATFORM}.${ARCHIVE_EXT}"
    tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t rwa-install)"
    archive_path="${tmpdir}/${archive}"
    checksums_path="${tmpdir}/SHA256SUMS.txt"
    bin_source="${tmpdir}/${BIN_PATH_IN_ARCHIVE}"

    trap 'rm -rf "$tmpdir"' EXIT INT TERM

    echo "Downloading pre-built binary: ${archive}"
    if ! download "${base_url}/${archive}" "$archive_path"; then
        echo "Pre-built binary not found for ${archive} (version: ${VERSION}). Falling back to source install." >&2
        rm -rf "$tmpdir"
        trap - EXIT INT TERM
        return 1
    fi

    if download "${base_url}/SHA256SUMS.txt" "$checksums_path"; then
        verify_checksum "$archive_path" "$checksums_path"
    else
        if [ "${RWA_INSTALL_INSECURE:-}" != "1" ]; then
            echo "ERROR: cannot verify download (SHA256SUMS.txt not found)." >&2
            echo "Fix: install coreutils (sha256sum) — or build from source:" >&2
            echo "  cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa" >&2
            echo "Or bypass verification at your own risk: RWA_INSTALL_INSECURE=1 <installer>" >&2
            exit 1
        fi
        echo "WARNING: RWA_INSTALL_INSECURE=1 set — installing WITHOUT checksum verification." >&2
    fi

    mkdir -p "$INSTALL_DIR"
    extract_archive "$archive_path" "$tmpdir"
    install -m 0755 "$bin_source" "$INSTALL_DIR/${BIN_PATH_IN_ARCHIVE}"

    rm -rf "$tmpdir"
    trap - EXIT INT TERM
    return 0
}

ensure_rust() {
    if need_cmd cargo; then
        return 0
    fi

    echo "Rust not found. Installing via rustup..."
    if ! need_cmd curl; then
        echo "Error: 'curl' is required to install Rust" >&2
        exit 1
    fi

    curl -fsSL https://sh.rustup.rs | sh -s -- -y --quiet
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
}

install_from_source() {
    tmproot="$(mktemp -d 2>/dev/null || mktemp -d -t rwa-source-install)"

    ensure_rust
    echo "Installing ${BIN_NAME} from source (ref: ${VERSION})..."
    case "$VERSION" in
        latest)
            cargo install --git "https://github.com/${REPO}" --bin "$BIN_NAME" --locked --root "$tmproot"
            ;;
        *)
            cargo install --git "https://github.com/${REPO}" --branch "$VERSION" --bin "$BIN_NAME" --locked --root "$tmproot"
            ;;
    esac || {
        rm -rf "$tmproot"
        return 1
    }

    mkdir -p "$INSTALL_DIR"
    install -m 0755 "$tmproot/bin/${BIN_PATH_IN_ARCHIVE}" "$INSTALL_DIR/${BIN_PATH_IN_ARCHIVE}"
    rm -rf "$tmproot"
}

show_summary() {
    installed_path="$1"

    echo ""
    echo "Installed: ${installed_path}"
    if [ -x "$installed_path" ]; then
        echo "Version:   $("$installed_path" --version 2>/dev/null || echo 'unknown')"
    fi

    echo ""
    echo "Quick start:"
    echo "  rwa keys generate --encrypt   # Create encrypted Solana wallet"
    echo "  rwa gm hours                  # Check market session"
    echo "  rwa gm buy TSLA 100 --dry-run # Preview a trade"
    echo ""
    echo "Agent skills: npx skills add outputlayer/rwa_skills -g -y"
    echo "Fund your wallet with SOL (transfers) and USDC (trading) to start."
}

main() {
    echo "Installing ${BIN_NAME} — trade tokenized stocks on Solana"
    echo ""

    detect_platform
    echo "Platform: ${PLATFORM}"
    echo "Version:  ${VERSION}"

    installed_path="${INSTALL_DIR}/${BIN_PATH_IN_ARCHIVE}"
    if ! install_prebuilt; then
        install_from_source
    fi

    show_summary "$installed_path"
}

main "$@"
