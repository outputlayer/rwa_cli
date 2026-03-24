use clap::Subcommand;
use eyre::Result;
use rwa_ondo::wallet::{self, Wallet};
use rwa_ondo::solana;

#[derive(Subcommand, Debug)]
pub enum KeysAction {
    /// Generate a new Solana wallet
    Generate,

    /// Import a wallet from key file, private key, or seed phrase
    Import {
        /// Path to a solana-keygen JSON key file
        #[arg(long)]
        file: Option<String>,

        /// Base58-encoded private key (64-byte keypair or 32-byte secret)
        #[arg(long)]
        private_key: Option<String>,

        /// BIP39 seed phrase (12 or 24 words in quotes)
        #[arg(long)]
        seed_phrase: Option<String>,
    },

    /// Show the active wallet address
    Show,

    /// Show SOL and USDC balances for the active wallet
    Balance,
}

pub async fn execute(action: KeysAction) -> Result<()> {
    match action {
        KeysAction::Generate => generate().await,
        KeysAction::Import { file, private_key, seed_phrase } => {
            import(file, private_key, seed_phrase).await
        }
        KeysAction::Show => show().await,
        KeysAction::Balance => balance().await,
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

async fn import(
    file: Option<String>,
    private_key: Option<String>,
    seed_phrase: Option<String>,
) -> Result<()> {
    let path = wallet::default_key_path()?;
    if path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists at {}.\nDelete it first if you want to import a new one.",
            path.display()
        ));
    }

    let w = match (file, private_key, seed_phrase) {
        (Some(f), None, None) => Wallet::from_file(std::path::Path::new(&f))?,
        (None, Some(pk), None) => Wallet::from_private_key(&pk)?,
        (None, None, Some(sp)) => Wallet::from_mnemonic(&sp)?,
        _ => return Err(eyre::eyre!(
            "Provide exactly one: --file, --private-key, or --seed-phrase"
        )),
    };

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

async fn balance() -> Result<()> {
    let w = Wallet::load_default()?;
    let pubkey = w.pubkey();

    let (sol, usdc) = tokio::join!(
        solana::get_sol_balance(&pubkey, None),
        solana::get_usdc_balance(&pubkey, None)
    );

    let sol = sol?;
    let usdc = usdc?;

    println!("Wallet:  {}", pubkey);
    println!("SOL:     {:.6}", sol);
    println!("USDC:    {:.2}", usdc);
    Ok(())
}
