#!/usr/bin/env python3
"""Regenerate crates/ondo/src/token_list.rs from live Ondo + Jupiter data.

The CLI keeps Solana mint addresses hardcoded (a verified, offline-deterministic
source of truth with canary tests) rather than resolving them at runtime — a
mistyped or spoofed mint moves money to the wrong place. This script automates
the *maintenance* of that hardcoded list; the mints stay hardcoded.

Pipeline:
  1. Fetch the official Ondo asset universe (symbols only — the API does not
     expose Solana mints).
  2. Keep tokenized equities/ETFs: symbol ends with "on" (excludes Ondo's own
     cash products OUSG/USDY) and is not on the non-equity blocklist (USDon).
  3. Resolve each symbol's mint via Jupiter token search, accepting a candidate
     ONLY when the symbol matches exactly AND its mintAuthority equals Ondo's
     issuer authority. This is what keeps memecoins ending in "on" out of the
     list — a spoof token cannot forge the issuer authority.
  4. Verify integrity (unique symbols/mints, base58 → 32 bytes, "on" suffix),
     sort byte-wise (matches Rust's &str sort), and rewrite token_list.rs.

Modes:
  (default)   rewrite token_list.rs in place
  --check     exit 1 if the generated list differs from the committed file
              (no write) — used by CI to detect drift

Network failures on individual symbols are retried; a symbol that never
resolves aborts the run (fail-closed) rather than silently dropping a token.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ONDO_ASSETS_URL = "https://app.ondo.finance/api/v2/assets"
ONDO_SESSION_URL = "https://status.ondo.finance/api/limits/session"
JUPITER_SEARCH_URL = "https://lite-api.jup.ag/tokens/v2/search?query="
SOLANA_RPC_URL = "https://api.mainnet-beta.solana.com"

# Solana Token-2022 program — every Ondo GM mint is owned by this program.
TOKEN_2022_PROGRAM = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"

# Ondo's token issuer (mint authority). Every genuine GM token is minted by this
# key; verifying it cryptographically excludes look-alike memecoins.
ONDO_MINT_AUTHORITY = "9foMHsSDq7nMg4WPusSz9eY7tyxyukqborA8GyU5cUxD"

# Ondo non-equity products that end with "on" but are NOT tradable stocks/ETFs.
# (OUSG/USDY are already excluded because they lack the "on" suffix.)
NON_EQUITY_BLOCKLIST = {"USDon"}

REPO_ROOT = Path(__file__).resolve().parent.parent
TOKEN_LIST_RS = REPO_ROOT / "crates" / "ondo" / "src" / "token_list.rs"

B58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def http_json(url: str, attempts: int = 6, timeout: int = 30):
    """GET + parse JSON with retries. HTTP 429 (rate limit) gets a longer
    exponential backoff than generic transient errors. Set RWA_JUPITER_API_KEY
    to raise Jupiter's rate limit (same env var the CLI uses)."""
    headers = {"User-Agent": "rwa-update-token-list"}
    api_key = os.environ.get("RWA_JUPITER_API_KEY", "").strip()
    if api_key and "jup.ag" in url:
        headers["x-api-key"] = api_key
    last = None
    for attempt in range(attempts):
        try:
            req = urllib.request.Request(url, headers=headers)
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            last = e
            # 429: back off hard (5s, 10s, 20s, 40s, 80s); other HTTP: short.
            time.sleep(min(5 * (2 ** attempt), 80) if e.code == 429 else 2.5)
        except Exception as e:  # noqa: BLE001 — transient network/DNS/timeout
            last = e
            time.sleep(2.5)
    raise RuntimeError(f"GET failed after {attempts} attempts: {url}\n  last error: {last}")


def rpc_post(method: str, params: list, attempts: int = 5, timeout: int = 40):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    last = None
    for _ in range(attempts):
        try:
            req = urllib.request.Request(
                SOLANA_RPC_URL, data=body,
                headers={"Content-Type": "application/json", "User-Agent": "rwa-update-token-list"})
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.load(r)["result"]
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(3)
    raise RuntimeError(f"Solana RPC {method} failed after {attempts} attempts: {last}")


def fetch_ondo_symbols() -> list[str]:
    data = http_json(ONDO_ASSETS_URL)
    assets = data["assets"]
    syms = [
        a["symbol"]
        for a in assets
        if a["symbol"].endswith("on") and a["symbol"] not in NON_EQUITY_BLOCKLIST
    ]
    print(f"Ondo API: {len(assets)} assets, {len(syms)} tokenized equities/ETFs after filter")
    return sorted(set(syms))


def resolve_mint(symbol: str) -> str:
    """Return the Solana mint for `symbol`, verified by Ondo's mint authority."""
    results = http_json(JUPITER_SEARCH_URL + urllib.parse.quote(symbol))
    if not isinstance(results, list):
        raise RuntimeError(f"{symbol}: unexpected Jupiter response shape")
    matches = [
        t
        for t in results
        if t.get("symbol") == symbol and t.get("mintAuthority") == ONDO_MINT_AUTHORITY
    ]
    if len(matches) == 1:
        return matches[0]["id"]
    if not matches:
        raise RuntimeError(
            f"{symbol}: no Jupiter token with exact symbol AND Ondo mint authority "
            f"(got symbols: {[t.get('symbol') for t in results[:5]]})"
        )
    raise RuntimeError(f"{symbol}: ambiguous — {len(matches)} Ondo-authority mints: "
                       f"{[t['id'] for t in matches]}")


def b58_byte_len(s: str) -> int:
    n = 0
    for c in s:
        if c not in B58_ALPHABET:
            return -1
        n = n * 58 + B58_ALPHABET.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    pad = len(s) - len(s.lstrip("1"))
    return pad + len(raw)


def verify(pairs: dict[str, str]) -> None:
    seen_mint: dict[str, str] = {}
    for sym, mint in pairs.items():
        if not sym.endswith("on"):
            sys.exit(f"INTEGRITY: symbol without 'on' suffix: {sym}")
        if b58_byte_len(mint) != 32:
            sys.exit(f"INTEGRITY: {sym}: mint is not base58→32 bytes: {mint}")
        if mint in seen_mint:
            sys.exit(f"INTEGRITY: duplicate mint {mint} for {seen_mint[mint]} and {sym}")
        seen_mint[mint] = sym


def verify_onchain(pairs: dict[str, str]) -> None:
    """Hard gate: every mint must exist on Solana mainnet as a Token-2022 mint
    whose authority is Ondo's issuer. A well-formed but wrong/garbage address
    (which the form checks in verify() would pass) fails here."""
    items = list(pairs.items())
    problems = []
    for k in range(0, len(items), 100):  # getMultipleAccounts caps at 100
        chunk = items[k:k + 100]
        vals = rpc_post("getMultipleAccounts",
                        [[m for _, m in chunk], {"encoding": "jsonParsed"}])["value"]
        for (sym, mint), v in zip(chunk, vals):
            if v is None:
                problems.append(f"{sym}: mint {mint} does not exist on Solana")
                continue
            if v["owner"] != TOKEN_2022_PROGRAM:
                problems.append(f"{sym}: not a Token-2022 mint (owner={v['owner']})")
            info = v.get("data", {}).get("parsed", {}).get("info", {})
            if info.get("mintAuthority") != ONDO_MINT_AUTHORITY:
                problems.append(f"{sym}: mintAuthority={info.get('mintAuthority')} != Ondo")
    if problems:
        sys.exit("ON-CHAIN CHECK FAILED:\n  " + "\n  ".join(problems))
    print(f"On-chain: all {len(pairs)} mints are Solana Token-2022 with Ondo authority ✓")


def verify_tradable(symbols: set[str]) -> None:
    """Hard gate: every symbol must appear in Ondo's session-limits (the
    authoritative tradability source the CLI itself uses) and be tradable in at
    least one session. A live Jupiter quote is NOT used here — these RWA tokens
    quote via RFQ market makers, so a route is intermittent (present one second,
    gone the next) and would make a flaky gate. Session-limits is per-session,
    stable, and definitive."""
    data = http_json(ONDO_SESSION_URL)
    limits = {x["symbol"]: x for x in data["limits"]}

    def any_session_tradable(x) -> bool:
        return any(isinstance(x.get(s), dict) and x[s].get("tradable")
                   for s in ("premarket", "regular", "postmarket", "overnight"))

    missing = sorted(s for s in symbols if s not in limits)
    never = sorted(s for s in symbols if s in limits and not any_session_tradable(limits[s]))
    if missing:
        sys.exit(f"TRADABILITY CHECK FAILED: not in Ondo session-limits: {missing}")
    if never:
        sys.exit(f"TRADABILITY CHECK FAILED: never tradable in any session: {never}")
    print(f"Tradability: all {len(symbols)} symbols are in Ondo session-limits, "
          f"tradable in ≥1 session ✓")


def render_rs(pairs: dict[str, str]) -> str:
    """Produce the full token_list.rs content with the new array + counts."""
    items = sorted(pairs.items(), key=lambda kv: kv[0].encode())  # byte-wise, == Rust &str sort
    lines = "\n".join(f'    ("{s}", "{m}"),' for s, m in items)
    count = len(items)

    src = TOKEN_LIST_RS.read_text()

    head = "static GM_TOKENS_STATIC: &[(&str, &str)] = &[\n"
    i = src.index(head) + len(head)
    j = src.index("\n];", i)
    src = src[:i] + lines + "\n" + src[j + 1:]

    # Update the "(N tokens)" doc comment and the count assertion/message.
    src = re.sub(r"Static fallback list of Ondo GM tokens \(\d+ tokens\)\.",
                 f"Static fallback list of Ondo GM tokens ({count} tokens).", src)
    src = re.sub(r"assert_eq!\(tokens\.len\(\), \d+, \"expected \d+ GM tokens\"\);",
                 f'assert_eq!(tokens.len(), {count}, "expected {count} GM tokens");', src)
    return src


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if token_list.rs would change; do not write")
    ap.add_argument("--delay", type=float, default=0.4,
                    help="seconds between Jupiter requests (rate-limit friendliness)")
    args = ap.parse_args()

    symbols = fetch_ondo_symbols()
    pairs: dict[str, str] = {}
    for idx, sym in enumerate(symbols, 1):
        pairs[sym] = resolve_mint(sym)
        if idx % 25 == 0 or idx == len(symbols):
            print(f"  resolved {idx}/{len(symbols)}")
        time.sleep(args.delay)

    verify(pairs)               # form: unique, base58→32 bytes, "on" suffix
    verify_onchain(pairs)       # on-chain: Solana Token-2022 + Ondo authority
    verify_tradable(set(pairs)) # tradability: Ondo session-limits, ≥1 session

    new_src = render_rs(pairs)
    old_src = TOKEN_LIST_RS.read_text()

    if new_src == old_src:
        print(f"token_list.rs is up to date ({len(pairs)} tokens). No changes.")
        return 0

    if args.check:
        print(f"DRIFT: token_list.rs is stale — {len(pairs)} tokens resolved differ "
              f"from the committed file. Run scripts/update-token-list.py to refresh.")
        return 1

    TOKEN_LIST_RS.write_text(new_src)
    print(f"Wrote {len(pairs)} tokens to {TOKEN_LIST_RS.relative_to(REPO_ROOT)}. "
          f"Run `cargo test -p rwa-ondo token_list` to validate.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
