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
        if [ "${RWA_INSTALL_INSECURE:-}" != "1" ]; then
            echo "ERROR: no checksum entry found for ${archive_name} in SHA256SUMS.txt." >&2
            echo "Fix: manifest may be corrupt or incomplete — or build from source:" >&2
            echo "  cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa" >&2
            echo "Or bypass verification at your own risk: RWA_INSTALL_INSECURE=1 <installer>" >&2
            exit 1
        fi
        echo "WARNING: RWA_INSTALL_INSECURE=1 set — installing WITHOUT checksum verification." >&2
        return 0
    fi

    # Every check below must signal failure EXPLICITLY (return 1 / exit 1), never
    # rely on `set -e`: verify_checksum runs inside install_prebuilt, which is
    # called via `if ! install_prebuilt` — and `set -e` is suppressed for the
    # whole call tree of a function used in an `if` condition. A bare failing
    # `sha256sum -c` there would print FAILED but NOT abort, installing a
    # tampered binary. So the mismatch path returns 1 and the caller acts on it.
    if need_cmd sha256sum; then
        if ! ( cd "$(dirname "$archive")" && sha256sum -c "$(basename "$checksum_file")" ); then
            echo "ERROR: checksum verification FAILED for ${archive_name}." >&2
            return 1
        fi
    elif need_cmd shasum; then
        if ! ( cd "$(dirname "$archive")" && shasum -a 256 -c "$(basename "$checksum_file")" ); then
            echo "ERROR: checksum verification FAILED for ${archive_name}." >&2
            return 1
        fi
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
    return 0
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
        # MUST check the result explicitly — see the note in verify_checksum:
        # `set -e` is suppressed here (install_prebuilt runs under `if !`), so a
        # bare call would let a checksum mismatch fall through to install.
        if ! verify_checksum "$archive_path" "$checksums_path"; then
            echo "ERROR: refusing to install — the download failed checksum verification." >&2
            echo "The archive may be corrupt or tampered. Try again, or build from source:" >&2
            echo "  cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa" >&2
            echo "Bypass at your own risk: RWA_INSTALL_INSECURE=1 <installer>" >&2
            exit 1
        fi
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

# Resolve the concrete latest release tag by following the releases/latest
# redirect, so a source fallback reproduces the RELEASE, not main HEAD.
# Best-effort: prints the tag on success, returns 1 if it can't be resolved.
resolve_latest_tag() {
    _latest_url="https://github.com/${REPO}/releases/latest"
    if need_cmd curl; then
        _final="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$_latest_url" 2>/dev/null)" || return 1
    elif need_cmd wget; then
        # awk exits 0 even when no Location line is found, so the pipeline's own
        # status can't gate this — the anchored case below is the real check
        # (an unresolved redirect yields an empty _final and returns 1).
        _final="$(wget -S --max-redirect=10 -O /dev/null "$_latest_url" 2>&1 \
            | awk '/^[[:space:]]*Location:/{loc=$2} END{print loc}')"
    else
        return 1
    fi
    # Anchor to the trusted repo's tag path — accept the tag only when the final
    # URL is exactly this repo's /releases/tag/<tag>, not any URL containing that
    # substring. (REPO is hardcoded, so even a bogus tag only makes the later
    # `cargo install --tag` fail against the real repo — fails safe, not open.)
    case "$_final" in
        "https://github.com/${REPO}/releases/tag/"*)
            printf '%s\n' "${_final##*/tag/}" ;;
        *) return 1 ;;
    esac
}

install_from_source() {
    tmproot="$(mktemp -d 2>/dev/null || mktemp -d -t rwa-source-install)"

    ensure_rust
    # NOTE: the source path's integrity rests on TLS + git ref pinning + --locked
    # (Cargo.lock), NOT on a SHA256 of a released artifact — categorically weaker
    # assurance than the checksum-verified prebuilt path above. H2/H3 tighten
    # WHICH ref is built (a real release tag, never a floating branch); they do
    # not add artifact verification.
    #
    # Pick the git ref explicitly. A TAG needs `--tag` (not `--branch`, which
    # cannot resolve a tag ref); only real branches use `--branch`.
    _ref_args=""
    case "$VERSION" in
        latest)
            _tag="$(resolve_latest_tag)" || _tag=""
            if [ -n "$_tag" ]; then
                echo "Resolved latest release tag: ${_tag}"
                _ref_args="--tag ${_tag}"
            else
                echo "WARNING: could not resolve the latest release tag; building the default branch (main HEAD)." >&2
            fi
            ;;
        main|master)
            _ref_args="--branch ${VERSION}"
            ;;
        *)
            _ref_args="--tag ${VERSION}"
            ;;
    esac
    echo "Installing ${BIN_NAME} from source (ref: ${VERSION})..."
    # shellcheck disable=SC2086
    cargo install --git "https://github.com/${REPO}" $_ref_args --bin "$BIN_NAME" --locked --root "$tmproot" || {
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

# Test seam: `RWA_INSTALL_NO_MAIN=1` lets the test harness source this file to
# exercise individual functions without running the installer.
if [ "${RWA_INSTALL_NO_MAIN:-}" != "1" ]; then
    main "$@"
fi
