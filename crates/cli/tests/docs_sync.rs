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
