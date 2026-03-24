use clap::Subcommand;
use eyre::Result;
use rwa_ondo::wallet::{self, Wallet};

#[derive(Subcommand, Debug)]
pub enum KeysAction {
    /// Generate a new Solana wallet
    Generate,

    /// Import a wallet from a solana-keygen JSON file
    Import {
        /// Path to the JSON key file
        #[arg(long)]
        file: String,
    },

    /// Show the active wallet address
    Show,
}

pub async fn execute(action: KeysAction) -> Result<()> {
    match action {
        KeysAction::Generate => generate().await,
        KeysAction::Import { file } => import(&file).await,
        KeysAction::Show => show().await,
    }
}

async fn generate() -> Result<()> {
    let path = wallet::default_key_path()?;
    if path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists at {}.\nDelete it first if you want to generate a new one.",
            path.display()
        ));
    }
    let w = Wallet::generate();
    let saved = w.save_default()?;
    println!("New wallet generated!");
    println!("Address:  {}", w.pubkey());
    println!("Key file: {}", saved.display());
    println!("\nFund this address with SOL and USDC to start trading.");
    Ok(())
}

async fn import(file: &str) -> Result<()> {
    let path = wallet::default_key_path()?;
    if path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists at {}.\nDelete it first if you want to import a new one.",
            path.display()
        ));
    }
    let w = Wallet::from_file(std::path::Path::new(file))?;
    let saved = w.save_default()?;
    println!("Wallet imported!");
    println!("Address:  {}", w.pubkey());
    println!("Key file: {}", saved.display());
    Ok(())
}

async fn show() -> Result<()> {
    let w = Wallet::load_default()?;
    println!("{}", w.pubkey());
    Ok(())
}
