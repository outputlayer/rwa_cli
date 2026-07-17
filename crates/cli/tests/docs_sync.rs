//! Drift guard: every CLI command must be documented. Fails CI when a new
//! command lands without a matching mention in the canonical command
//! references (README.md and CLAUDE.md). Does NOT cover the separate
//! `rwa_skills` repo — see the failure message.

use clap::CommandFactory;
use rwa_cli::Cli;

/// Command names the CLI exposes: top-level commands plus every `gm`/`keys`
/// leaf subcommand (clap renders these kebab-case, e.g. `close-all`).
fn command_names() -> Vec<String> {
    let cmd = Cli::command();
    let mut names = Vec::new();
    for sub in cmd.get_subcommands() {
        match sub.get_name() {
            "gm" | "keys" => {
                for leaf in sub.get_subcommands() {
                    names.push(leaf.get_name().to_string());
                }
            }
            other => names.push(other.to_string()),
        }
    }
    names
}

fn doc(file: &str) -> String {
    let path = format!("{}/../../{}", env!("CARGO_MANIFEST_DIR"), file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

#[test]
fn every_command_is_documented() {
    let docs = [("README.md", doc("README.md")), ("CLAUDE.md", doc("CLAUDE.md"))];
    let mut missing = Vec::new();
    for name in command_names() {
        for (file, content) in &docs {
            if !content.contains(&name) {
                missing.push(format!("  `{name}` is not mentioned in {file}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "Undocumented CLI commands — update README.md / CLAUDE.md (and the rwa_skills repo if relevant):\n{}",
        missing.join("\n")
    );
}

/// Drift guard for the AGENT CONTRACT: every `GmTradeErrorKind` label AND
/// every `ExecuteFailureKind` label (Jupiter `/execute` failures — also
/// surfaced via `classify_error` into the `error_kind` JSON field, see M2/L6)
/// must be documented in llms.txt (the agent manual), README.md, and
/// CLAUDE.md. Seven kinds were added in 0.7.2 alone; before this guard a new
/// one reached agents undocumented if the author forgot the three prose
/// lists. The source lists are `GmTradeErrorKind::ALL` and
/// `ExecuteFailureKind::ALL`, each kept exhaustive by a compile-time tripwire
/// in the ondo crate.
#[test]
fn error_kinds_are_documented() {
    use rwa_ondo::jupiter::ExecuteFailureKind;
    use rwa_ondo::usecases::gm::GmTradeErrorKind;
    let docs = [
        ("llms.txt", doc("llms.txt")),
        ("README.md", doc("README.md")),
        ("CLAUDE.md", doc("CLAUDE.md")),
    ];
    let mut missing = Vec::new();
    let mut labels: Vec<&'static str> = GmTradeErrorKind::ALL.iter().map(|k| k.label()).collect();
    labels.extend(ExecuteFailureKind::ALL.iter().map(|k| k.label()));
    for label in labels {
        // A plain `contains` is a weak check for labels that are substrings of
        // OTHER documented labels — `unknown` lives inside `unknown_wallet`,
        // `unknown_token`, `unknown_aggregator_error`, etc., so a bare
        // `contains("unknown")` would pass even if `unknown` itself were never
        // documented. For those, require the exact backtick-quoted form so the
        // guard actually proves the standalone label is present.
        let needle = if label == "unknown" {
            "`unknown`".to_string()
        } else {
            label.to_string()
        };
        for (file, content) in &docs {
            if !content.contains(&needle) {
                missing.push(format!("  error_kind `{label}` is not documented in {file}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "Undocumented error kinds — agents branch on these; add to llms.txt / README.md / CLAUDE.md:\n{}",
        missing.join("\n")
    );
}
