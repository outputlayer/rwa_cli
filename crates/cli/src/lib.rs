pub mod cmd;

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
}

fn lock_path() -> Result<PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| eyre!("Cannot determine config directory"))?;
    let dir = config.join("rwa");
    fs::create_dir_all(&dir)?;
    Ok(dir.join(".lock"))
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
            let err = serde_json::json!({"status": "error", "error": msg});
            println!("{err}");
            std::process::exit(1);
        }
        return Err(eyre!(msg));
    }

    // Lock is held until lock_file is dropped (end of process)
    let result = match cli.command {
        Commands::Gm { action } => cmd::gm::execute(action, json, cli.rpc_url.as_deref()).await,
        Commands::Keys { action } => cmd::keys::execute(action).await,
    };

    // Explicit unlock (also happens on drop, but be clear)
    let _ = lock_file.unlock();
    result
}
