pub mod cmd;

use clap::Parser;
use eyre::Result;

/// RWA CLI — interact with Real World Asset protocols from the terminal.
#[derive(Parser, Debug)]
#[command(name = "rwa", version, about = "Real World Asset CLI")]
pub struct Cli {
    /// Custom RPC URL (overrides default for the selected chain)
    #[arg(long, global = true, env = "RWA_RPC_URL")]
    pub rpc_url: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(clap::Subcommand, Debug)]
pub enum Commands {
    /// Ondo Global Markets — 264 tokenized stocks & ETFs on Solana, BNB & Ethereum (list, price, balance, info, portfolio)
    Gm {
        #[command(subcommand)]
        action: cmd::gm::GmAction,
    },
}

/// Run the CLI.
pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Gm { action } => cmd::gm::execute(action, cli.rpc_url.as_deref()).await?,
    }

    Ok(())
}
