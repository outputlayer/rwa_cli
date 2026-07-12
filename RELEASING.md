# Releasing

The release pipeline, so it doesn't live only in one person's head.

## Cut a release

```bash
# 0. On main, tree clean, everything merged.
git checkout main && git pull

# 1. Write the CHANGELOG entry (newest first, under a new "## [X.Y.Z] - DATE" heading).
#    Keep the JSON-contract-stability promise in the header: patch releases add
#    optional fields but never remove/rename one; breaking JSON/flag/exit-code
#    changes need a MINOR bump and a "Breaking" section.

# 2. Bump the workspace version.
sed -i '' 's/^version = "X.Y.Z-1"/version = "X.Y.Z"/' Cargo.toml

# 3. Rebuild so Cargo.lock picks up the new version, then run the FULL gate.
cargo build --release
make ci                     # exact mirror of the gating CI jobs; MUST be green before pushing

# 4. Commit, push main, tag, push the tag.
git add -A && git commit -m "release: vX.Y.Z — <one line>"
git push origin main
git tag vX.Y.Z && git push origin vX.Y.Z
```

Pushing the `vX.Y.Z` tag triggers `.github/workflows/release.yml`, which builds the
five platform archives, attests their build provenance, writes `SHA256SUMS.txt`,
and creates the GitHub Release.

## Verify the release

```bash
# Wait for the 6 assets (5 archives + SHA256SUMS) to appear:
gh release view vX.Y.Z --json assets -q '.assets[].name'

# Install locally and smoke-check:
cargo install --path bin/rwa
rwa --version                # should print X.Y.Z
rwa update --check           # should say "Already up to date (vX.Y.Z)"

# (optional) verify provenance of a published asset:
gh attestation verify <downloaded-asset> --repo outputlayer/rwa_cli
```

## Sync the skills repo

Agent-facing changes (new/changed commands, `error_kind`s, JSON fields, flag
behavior) must be mirrored into the sibling **`~/DevProjects/rwa/rwa_skills`**
repo (`skills/rwa-{trade,wallet,portfolio}/SKILL.md`). It has its own
pre-commit markdownlint (the "<80 lines" note on SKILL.md is a warning, not a
blocker). Its remote is often ahead — `git pull --rebase` first.

Pure-internal changes (refactors, dependency bumps, CI/test-only) need **no**
skills sync — the agent contract is unchanged.

## Rules of thumb

- **`make ci` before every push**, always. Plain `cargo test`/`cargo clippy`
  (no `-Dwarnings`, plus clippy's cache) can be green while CI is red.
- Patch (`0.7.x`) = fixes + additive JSON. Minor (`0.x.0`) = any breaking
  change to the agent contract (removed flag, renamed JSON field, changed
  `error_kind`/exit code). The `--parallel` flag removal in 0.7.2 is the one
  place this was stretched (a no-op flag) — don't repeat it without a minor bump.
- The `Security Audit` CI job (`cargo audit`) is NOT part of `make ci` — it needs
  the advisory DB and can go red on a newly-published advisory with no code
  change. Address advisories with `cargo update` (patch) or a documented
  `--ignore` (unmaintained build/dev-only crates), never by silencing the job.
