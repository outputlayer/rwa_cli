# Local mirror of the GitHub CI pipeline (.github/workflows/ci.yml).
#
# CI runs EVERY step with RUSTFLAGS=-Dwarnings, so warnings — clippy lints,
# `unsafe_code`, dead_code, etc. — are hard errors there. Plain local
# `cargo test` / `cargo clippy` (without -Dwarnings) pass while CI fails, and
# `cargo clippy`'s incremental cache can hide a warning in an unchanged file.
# Run `make ci` before every push so what's green locally is green in CI.

export RUSTFLAGS := -Dwarnings

.PHONY: ci check lint test integration release fmt install-hooks test-install mutants mutants-list

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

# ── Mutation testing ─────────────────────────────────────────────────────────
#
# Checks the TESTS, not the code: cargo-mutants breaks the source (>= for >,
# a constant return, * for /) and a mutant that SURVIVES means no test noticed.
# Deliberately NOT part of `make ci` — a run costs minutes per file, while
# `make ci` must stay a fast mirror of ci.yml.
#
# Install once:  cargo install cargo-mutants
#
# `-p <package>` is MANDATORY. Without it cargo-mutants only sees the workspace
# root binary (bin/rwa/src/main.rs, 2 mutants) and reports green having checked
# essentially nothing — the exact false confidence this target exists to prevent.
#
# Not every survivor is a gap. `x.abs() > f64::EPSILON` mutated to `>=` diverges
# only at exactly 2.22e-16; a test for that pins a value no scenario produces.
# Record such equivalent mutants with a reason instead of chasing 100%.

## mutants — mutation-test one file: make mutants FILE=close_all.rs [PKG=rwa-cli]
mutants:
	@test -n "$(FILE)" || { echo "usage: make mutants FILE=view.rs [PKG=rwa-cli]"; exit 2; }
	cargo mutants -p $(or $(PKG),rwa-cli) --file "**/$(FILE)" -j 4 --timeout 120

## mutants-list — list mutants without running them (cheap: shows what is checkable)
mutants-list:
	@test -n "$(FILE)" || { echo "usage: make mutants-list FILE=view.rs [PKG=rwa-cli]"; exit 2; }
	cargo mutants -p $(or $(PKG),rwa-cli) --file "**/$(FILE)" --list

## fmt — format (not gated by CI, but keep the tree tidy)
fmt:
	cargo fmt --all

## test-install — hermetic fail-closed checksum test for install.sh (not in `make ci`,
## mirrors the CI "Install Script" job; no network/build needed)
test-install:
	sh scripts/test-install.sh

## install-hooks — enable the pre-push hook that runs `make ci`
install-hooks:
	git config core.hooksPath .githooks
	@echo "pre-push hook enabled → 'make ci' runs before every push (bypass: git push --no-verify)"
