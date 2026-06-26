pub mod cmd;
pub mod wallets;

use clap::Parser;
use eyre::{Result, eyre};
use fs2::FileExt;
use std::fs;
use std::path::PathBuf;

/// RWA CLI — trade tokenized stocks & ETFs (Ondo GM) on Solana.
#[derive(Parser, Debug)]
#[command(name = "rwa", version, about = "Real World Asset CLI")]
pub struct Cli {
    /// Custom RPC URL (overrides default for the selected chain)
    #[arg(long, global = true, env = "RWA_RPC_URL")]
    pub rpc_url: Option<String>,

    /// Output JSON instead of human-readable text (for agents/scripts)
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(clap::Subcommand, Debug)]
pub enum Commands {
    /// Ondo Global Markets — tokenized stocks & ETFs on Solana
    Gm {
        #[command(subcommand)]
        action: cmd::gm::GmAction,
    },
    /// Solana wallet management (generate, import, show)
    Keys {
        #[command(subcommand)]
        action: cmd::keys::KeysAction,
    },
    /// Update rwa to the latest release
    Update {
        /// Only check for a newer version; don't replace the binary
        #[arg(long)]
        check: bool,
        /// Skip the confirmation prompt and update immediately
        #[arg(short, long)]
        yes: bool,
    },
}

fn lock_path() -> Result<PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| eyre!("Cannot determine config directory"))?;
    let dir = config.join("rwa");
    fs::create_dir_all(&dir)?;
    Ok(dir.join(".lock"))
}

/// Render a top-level CLI error, attaching a stable `error_kind` when the error
/// is a known structured type (e.g. a Jupiter `ExecuteFailure` or a GM trade
/// error). Centralized here — where `classify_error` is in scope — so every
/// command, including a single `gm buy`/`sell`, surfaces the same JSON contract
/// and the full cause chain instead of only the outermost wrap message.
pub fn render_error(err: &eyre::Error, json: bool) {
    if json {
        let obj = serde_json::json!({
            "status": "error",
            // `{:#}` joins the full eyre cause chain ("wrap: cause: root"),
            // so the real failure is visible, not just "swap execution failed".
            "error": format!("{err:#}"),
            "error_kind": rwa_ondo::usecases::gm::classify_error(err),
        });
        println!("{obj}");
    } else {
        eprintln!("Error: {err:#}");
    }
}

/// Exit code for a top-level error: 75 (EX_TEMPFAIL) for transient failures
/// worth retrying (RPC/exchange hiccups, confirmation timeouts), 1 otherwise.
/// Documented contract for scripts/agents alongside the JSON `error_kind`.
#[must_use]
pub fn exit_code_for(err: &eyre::Error) -> i32 {
    match rwa_ondo::usecases::gm::classify_error(err) {
        Some(kind) if rwa_ondo::usecases::gm::is_transient_kind(kind) => 75,
        _ => 1,
    }
}

/// Run the CLI.
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;

    // Acquire exclusive file lock — prevents concurrent rwa processes
    // (Jupiter rejects parallel requests from the same wallet)
    let lock_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path()?)?;

    if lock_file.try_lock_exclusive().is_err() {
        let msg = "Another rwa process is running. Jupiter rejects concurrent requests from the same wallet — wait for it to finish.";
        if json {
            render_error(&eyre!(msg), true);
            // Lock contention is transient by definition — retry when the
            // other process finishes.
            std::process::exit(75);
        }
        return Err(eyre!(msg));
    }

    // Lock is held until lock_file is dropped (end of process)
    let result = match cli.command {
        Commands::Gm { action } => cmd::gm::execute(action, json, cli.rpc_url.as_deref()).await,
        Commands::Keys { action } => cmd::keys::execute(action).await,
        Commands::Update { check, yes } => cmd::update::run(check, yes, json).await,
    };

    // Explicit unlock (also happens on drop, but be clear)
    let _ = lock_file.unlock();
    result
}

#[cfg(test)]
mod exit_code_tests {
    use super::exit_code_for;

    #[test]
    fn transient_kinds_exit_75() {
        let err: eyre::Report = rwa_ondo::solana::TransactionError {
            kind: rwa_ondo::solana::TransactionErrorKind::ConfirmationTimeout,
            detail: "tx may still land".into(),
        }
        .into();
        assert_eq!(exit_code_for(&err), 75);
    }

    #[test]
    fn permanent_and_unclassified_exit_1() {
        assert_eq!(exit_code_for(&eyre::eyre!("No wallet found")), 1);
    }
}
