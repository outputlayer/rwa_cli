# Changelog

All notable changes to this project will be documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versioning follows [Semantic Versioning](https://semver.org/).

**JSON contract stability:** once a field appears in a release, it is not removed or renamed in a patch release. New optional fields may be added. Breaking JSON changes require a minor version bump and are listed under **Breaking**.

---

## [0.2.0] — 2026-03-29

### Breaking

- **Portfolio JSON restructured.** `sol`, `usdc`, `total_value_usd`, `change_24h_usd`, `change_24h_pct` are no longer top-level. New shape:
  ```json
  {
    "wallet": "...",
    "cash": { "sol": 0.087, "usdc": 0.00 },
    "gm_positions": {
      "positions": [...],
      "value_usd": 0.00,
      "change_24h_usd": 0.00,
      "change_24h_pct": 0.00
    }
  }
  ```
- **`rwa gm quote` removed.** Use `rwa gm buy <SYM> <AMT> --dry-run` instead — it validates balance and tradability in addition to returning the full quote (`slippage_pct`, `price_impact_pct`, `fee_bps`, `gasless`, `router`).
- **`alloc_pct` renamed to `gm_alloc_pct`** in position objects — reflects that allocation is within GM positions only, not total portfolio.

### Added

- `--dry-run` flag on `buy`, `sell`, `send`, `close-all` — validates and shows quote without executing.
- `buy/sell --dry-run` JSON now includes `price_impact_pct` and `fee_bps` (previously only in `quote`).
- Wallet encryption via [age](https://age-encryption.org/):
  - `rwa keys generate --encrypt` — create encrypted wallet (`key.age`)
  - `rwa keys encrypt` / `decrypt` — convert between `key.json` ↔ `key.age`
  - `RWA_PASSPHRASE` env var for scripted/agent access
- `zeroize` on all in-memory secret key material.
- Integration tests with `httpmock` for portfolio RPC parsing (standard, out-of-order batch, malformed entries, empty, error propagation, sort order).
- Property-based tests (`proptest`) for `token_to_raw` / `format_amount` roundtrips.
- Typed error kinds surfaced in JSON: `market_closed`, `not_tradable`, `slippage_too_high`, `confirmation_timeout`, `on_chain_failure`.
- `amounts` module — centralized raw↔display amount parsing and formatting.
- `usecases::gm` module — trade logic extracted from CLI layer (prepare/execute split).
- CI job split: `check`, `unit tests (ondo)`, `unit tests (cli)`, `integration tests`, `release build`.

### Fixed

- RPC batch responses now sorted by `id` before processing — handles out-of-order delivery per JSON-RPC 2.0 spec.
- Custom `--rpc-url` no longer appends public fallback URLs (prevents silent failover to wrong endpoint).
- Removed extrnode (401) and drpc (400) from default RPC fallback list — only `mainnet-beta.solana.com` and `publicnode` remain.
- SOL `send all` now works on raw lamports — no float precision loss on exact drain.
- Portfolio crash on RPC nodes returning errors in batch response.
- Malformed batch entries silently skipped instead of crashing.

### Changed

- Workspace edition: `2021` → `2024`.
- Version: `0.1.0` → `0.2.0`.

---

## [0.1.0] — 2025-12-01

Initial release.

- `rwa gm buy / sell / close-all / portfolio / history / list / send / reclaim / hours`
- `rwa keys generate / import / show`
- Jupiter Ultra gasless swaps, RFQ routing
- Token-2022 support for Ondo GM tokens
- SLIP-10 mnemonic derivation (m/44'/501'/0'/0')
- Solana RPC with retry and URL rotation
