#!/usr/bin/env bash
#
# Real-world latency benchmark for rwa commands.
#
# Measures wall-clock latency (p50/p95/min/max) of read-only commands over N
# runs, plus a sequential-vs-parallel comparison for basket dry-runs. These are
# NETWORK-BOUND (Jupiter + Solana RPC), so absolute numbers depend on your
# connectivity and Jupiter/RPC load — use them to compare runs, not as hard SLAs.
#
# Usage:
#   bash scripts/bench-latency.sh [N]        # N runs per command (default 8)
#   RWA_BIN=./target/release/rwa bash scripts/bench-latency.sh 12
#
# Only read-only/dry-run commands are exercised — nothing spends funds.

set -euo pipefail

RWA="${RWA_BIN:-rwa}"
N="${1:-8}"

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# Read whitespace-separated ms values on stdin, print "p50 p95 min max".
pctl() {
  python3 -c '
import sys, math
v = sorted(float(x) for x in sys.stdin.read().split())
def p(q):
    if not v: return 0.0
    i = max(0, min(len(v) - 1, math.ceil(q / 100 * len(v)) - 1))
    return v[i]
print(f"{p(50):.0f} {p(95):.0f} {min(v):.0f} {max(v):.0f}")'
}

bench() {
  local label="$1"; shift
  local times=() s e i
  for i in $(seq 1 "$N"); do
    s=$(now_ms)
    "$@" >/dev/null 2>&1 || true
    e=$(now_ms)
    times+=("$((e - s))")
  done
  read -r p50 p95 mn mx < <(printf '%s\n' "${times[@]}" | pctl)
  printf '  %-28s p50=%5sms  p95=%5sms  min=%5sms  max=%5sms\n' "$label" "$p50" "$p95" "$mn" "$mx"
}

echo "rwa latency benchmark — binary: $RWA, N=$N runs/command"
"$RWA" --version 2>/dev/null || true
echo "NOTE: network-bound (Jupiter / Solana RPC); numbers vary with connectivity."
echo

echo "Read-only commands:"
bench "gm hours"             "$RWA" --json gm hours
bench "gm list"              "$RWA" --json gm list
bench "gm tradable NVDA"     "$RWA" --json gm tradable NVDA
bench "gm buy NVDA 40 (dry)" "$RWA" --json gm buy NVDA 40 --dry-run
bench "gm portfolio"         "$RWA" --json gm portfolio
echo

echo "Basket dry-run — sequential vs --parallel (3 tokens):"
bench "buy-basket seq (dry)" "$RWA" --json gm buy-basket AAPL 10 TSLA 10 NVDA 10 --dry-run
bench "buy-basket par (dry)" "$RWA" --json gm buy-basket AAPL 10 TSLA 10 NVDA 10 --parallel --dry-run
echo
echo "Done. (Parallel basket should show lower p50 than sequential when the"
echo " network round-trips dominate.)"
