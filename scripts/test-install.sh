#!/bin/sh
# Hermetic tests for install.sh's fail-closed checksum verification (audit H1).
# No network: `download` is stubbed to serve local fixtures, and install_prebuilt
# is run in a subshell so a fail-closed `exit 1` is observed, not propagated.
#
# Run: sh scripts/test-install.sh   (exit 0 = all pass)
set -u

REPO_ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
INSTALL_SH="${REPO_ROOT}/install.sh"

pass=0
fail=0
ok()   { pass=$((pass + 1)); echo "ok   - $1"; }
bad()  { fail=$((fail + 1)); echo "FAIL - $1"; }

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
    else shasum -a 256 "$1" | awk '{print $1}'; fi
}

# One test run: builds a fixture dir, stubs download() to copy fixtures, runs
# install_prebuilt in a subshell, returns its exit code; sets INSTALLED_OK=1 if
# the "binary" actually landed in the fake INSTALL_DIR.
run_case() {
    _mode="$1"  # good | tampered
    work="$(mktemp -d)"
    fixtures="${work}/fixtures"
    mkdir -p "$fixtures"

    # Fake archive named for whatever platform the host resolves to.
    OS="$(uname -s)"; ARCH="$(uname -m)"
    case "$OS" in Linux) o=unknown-linux-gnu;; Darwin) o=apple-darwin;; *) o=apple-darwin;; esac
    case "$ARCH" in x86_64|amd64) a=x86_64;; aarch64|arm64) a=aarch64;; *) a=x86_64;; esac
    plat="${a}-${o}"
    # A tar.gz containing a `rwa` executable, so extract+install would succeed if reached.
    bindir="${work}/bin"; mkdir -p "$bindir"
    printf '#!/bin/sh\necho rwa 9.9.9\n' > "${bindir}/rwa"; chmod +x "${bindir}/rwa"
    ( cd "$bindir" && tar -czf "${fixtures}/rwa-${plat}.tar.gz" rwa )

    real_hash="$(sha256_of "${fixtures}/rwa-${plat}.tar.gz")"
    if [ "$_mode" = tampered ]; then
        manifest_hash="0000000000000000000000000000000000000000000000000000000000000000"
    else
        manifest_hash="$real_hash"
    fi
    printf '%s  rwa-%s.tar.gz\n' "$manifest_hash" "$plat" > "${fixtures}/SHA256SUMS.txt"

    export RWA_INSTALL_NO_MAIN=1
    export INSTALL_DIR="${work}/dest"
    export RWA_FIXTURES="$fixtures"
    export VERSION=latest

    # Run in a sourcing subshell; use `if` so errexit/`exit 1` in the callee is
    # captured cleanly as pass/fail instead of aborting the capture.
    # shellcheck disable=SC1090
    ec="$(
        . "$INSTALL_SH"
        # Stub the network: serve the requested basename from the fixtures dir.
        download() { cp "${RWA_FIXTURES}/$(basename "$2")" "$2" 2>/dev/null; }
        detect_platform
        if ( install_prebuilt ) >/dev/null 2>&1; then echo 0; else echo 1; fi
    )"

    INSTALLED_OK=0
    [ -x "${work}/dest/rwa" ] && INSTALLED_OK=1
    LAST_EC="$ec"
    rm -rf "$work"
}

# 1. Tampered archive (hash mismatch): must fail closed, nothing installed.
run_case tampered
if [ "$LAST_EC" != "0" ] && [ "$INSTALLED_OK" = "0" ]; then
    ok "tampered archive: install aborts (exit $LAST_EC) and no binary installed"
else
    bad "tampered archive: expected non-zero exit + no install, got exit=$LAST_EC installed=$INSTALLED_OK"
fi

# 2. Valid archive (hash matches): must succeed and install the binary.
run_case good
if [ "$LAST_EC" = "0" ] && [ "$INSTALLED_OK" = "1" ]; then
    ok "valid archive: install succeeds and binary is placed"
else
    bad "valid archive: expected exit 0 + install, got exit=$LAST_EC installed=$INSTALLED_OK"
fi

echo ""
echo "install.sh tests: ${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
