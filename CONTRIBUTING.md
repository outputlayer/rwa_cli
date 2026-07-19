# Contributing to rwa

Thanks for your interest! This is a small, safety-focused CLI that moves real money — the bar for changes is "does this make money movement safer or agent behavior clearer", not feature count. Read the [Anti-Overengineering Checklist in CLAUDE.md](CLAUDE.md#anti-overengineering-checklist) before proposing a feature.

## Development setup

Stable Rust toolchain, no native C dependencies (the repo stays pure Rust).

```bash
git clone https://github.com/outputlayer/rwa_cli.git
cd rwa_cli
make ci                # REQUIRED before every push — exact mirror of .github/workflows/ci.yml
make install-hooks     # optional: pre-push hook that runs `make ci` automatically
```

CI runs everything with `RUSTFLAGS=-Dwarnings`, so a plain local `cargo test`/`cargo clippy` can be green while CI is red. `make ci` is the source of truth.

## What CI cannot verify

The live on-chain money paths (`usecases/gm_execute.rs`, `jupiter/execute.rs` submit, `solana/transfer.rs` send, `usecases/gm_gas.rs` auto-refuel) never run in CI — no wallet, no live Jupiter/RPC. If your change touches any of them, a green `make ci` is necessary but **not sufficient**: validate with a live `--dry-run` followed by a small real `-y` trade before merging. Everything else (quote math, gates, parsing, ledger/pnl, JSON shapes) is covered by tests.

## Conventions

- Errors: `eyre`; no `.unwrap()` on fallible runtime paths
- Dependencies live in the root `[workspace.dependencies]`
- Wallet-changing commands stay sequential; avoid bursts of concurrent Solana RPC calls
- Full product/code conventions live in [CLAUDE.md](CLAUDE.md) — it is the canonical spec and must not drift from the CLI, README, or `llms.txt`

## Stability contracts

JSON output and exit codes (75 = transient/retry, 1 = permanent) are a stable contract for scripts and agents. Once a JSON field ships in a release it is not removed or renamed in a patch release; breaking changes require a minor version bump and a **Breaking** entry in [CHANGELOG.md](CHANGELOG.md).

## Pull requests

1. Branch from `main`, keep the change small and single-purpose
2. `make ci` green
3. Update `CHANGELOG.md` and any affected docs (README, CLAUDE.md, `llms.txt`) in the same PR
4. For money-path changes, note in the PR description how the live validation was done

Releases are cut by maintainers per [RELEASING.md](RELEASING.md).

## Security issues

Please report vulnerabilities privately via [GitHub Security Advisories](https://github.com/outputlayer/rwa_cli/security/advisories/new), not public issues — this tool holds private keys and moves funds.
