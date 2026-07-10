# Local mirror of the GitHub CI pipeline (.github/workflows/ci.yml).
#
# CI runs EVERY step with RUSTFLAGS=-Dwarnings, so warnings — clippy lints,
# `unsafe_code`, dead_code, etc. — are hard errors there. Plain local
# `cargo test` / `cargo clippy` (without -Dwarnings) pass while CI fails, and
# `cargo clippy`'s incremental cache can hide a warning in an unchanged file.
# Run `make ci` before every push so what's green locally is green in CI.

export RUSTFLAGS := -Dwarnings

.PHONY: ci check lint test integration release fmt install-hooks

# NOTE: `make ci` mirrors ci.yml's BUILD/TEST/LINT jobs (the ones a code change
# can break). The separate `audit` job (cargo-audit / RUSTSEC advisory scan) is
# NOT mirrored here: it needs the advisory DB and can go red on a newly-published
# advisory with no code change, so it must not gate a local push.

## ci — full CI-parity gate; run before every push (mirrors ci.yml's build/test jobs)
ci: check lint test integration release
	@echo "✅ CI-parity gate passed — matches .github/workflows/ci.yml"

## check — cargo check (the "Check & Lint" job, part 1)
check:
	cargo check --workspace

## lint — clippy on all targets (the "Check & Lint" job, part 2)
lint:
	cargo clippy --all-targets

## test — unit tests, per crate, as CI runs them
test:
	cargo test -p rwa-ondo --lib
	cargo test -p rwa-cli --lib

## integration — the mock-backed integration + JSON-contract + docs-drift suites
integration:
	cargo test -p rwa-ondo --test rpc_portfolio
	cargo test -p rwa --test cli_contract
	cargo test -p rwa-cli --test docs_sync

## release — release build (the gating "Release Build" job)
release:
	cargo build --release

## fmt — format (not gated by CI, but keep the tree tidy)
fmt:
	cargo fmt --all

## install-hooks — enable the pre-push hook that runs `make ci`
install-hooks:
	git config core.hooksPath .githooks
	@echo "pre-push hook enabled → 'make ci' runs before every push (bypass: git push --no-verify)"
