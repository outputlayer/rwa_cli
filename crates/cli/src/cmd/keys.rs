use clap::Subcommand;
use eyre::Result;
use rwa_ondo::wallet::{self, Wallet};

#[derive(Subcommand, Debug)]
pub enum KeysAction {
    /// Generate a new Solana wallet
    Generate {
        /// Encrypt the wallet with a passphrase (saves as key.age instead of key.json)
        #[arg(long)]
        encrypt: bool,
    },

    /// Import a wallet from key file, private key, or seed phrase
    Import {
        /// Path to a solana-keygen JSON key file
        #[arg(long)]
        file: Option<String>,

        /// Base58-encoded private key (64-byte keypair or 32-byte secret)
        #[arg(long)]
        private_key: Option<String>,

        /// BIP39 seed phrase (12 or 24 words). Omit value to enter interactively (safer).
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        seed_phrase: Option<String>,

        /// Encrypt the saved wallet with a passphrase (saves as key.age instead of key.json)
        #[arg(long)]
        encrypt: bool,
    },

    /// Show the active wallet address
    Show,

    /// Encrypt the wallet (key.json → key.age, then removes key.json)
    Encrypt,

    /// Decrypt the wallet (key.age → key.json, then removes key.age)
    Decrypt,
}

pub async fn execute(action: KeysAction) -> Result<()> {
    match action {
        KeysAction::Generate { encrypt } => generate(encrypt).await,
        KeysAction::Import { file, private_key, seed_phrase, encrypt } => {
            import(file, private_key, seed_phrase, encrypt).await
        }
        KeysAction::Show => show().await,
        KeysAction::Encrypt => encrypt_wallet().await,
        KeysAction::Decrypt => decrypt_wallet().await,
    }
}

async fn generate(encrypt: bool) -> Result<()> {
    let json_path = wallet::default_key_path()?;
    let age_path = wallet::encrypted_key_path()?;
    if json_path.exists() || age_path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists. Delete it first if you want to generate a new one."
        ));
    }
    let w = Wallet::generate();
    if encrypt {
        let passphrase = prompt_new_passphrase()?;
        let saved = w.save_default_encrypted(&passphrase)?;
        println!("New wallet generated (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    } else {
        let saved = w.save_default()?;
        println!("New wallet generated!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
        println!("\nFund this address with SOL and USDC to start trading.");
    }
    Ok(())
}

async fn import(
    file: Option<String>,
    private_key: Option<String>,
    seed_phrase: Option<String>,
    encrypt: bool,
) -> Result<()> {
    let json_path = wallet::default_key_path()?;
    let age_path = wallet::encrypted_key_path()?;
    if json_path.exists() || age_path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists. Delete it first if you want to import a new one."
        ));
    }

    let w = match (file, private_key, seed_phrase) {
        (Some(f), None, None) => Wallet::from_file(std::path::Path::new(&f))?,
        (None, Some(pk), None) => Wallet::from_private_key(&pk)?,
        (None, None, Some(sp)) => {
            let phrase = if sp.is_empty() {
                let input = rpassword::prompt_password("Enter seed phrase: ")
                    .map_err(|e| eyre::eyre!("Failed to read seed phrase: {e}"))?;
                if input.is_empty() {
                    return Err(eyre::eyre!("Seed phrase cannot be empty"));
                }
                input
            } else {
                sp
            };
            Wallet::from_mnemonic(&phrase)?
        }
        _ => return Err(eyre::eyre!(
            "Provide exactly one: --file, --private-key, or --seed-phrase"
        )),
    };

    if encrypt {
        let passphrase = prompt_new_passphrase()?;
        let saved = w.save_default_encrypted(&passphrase)?;
        println!("Wallet imported (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    } else {
        let saved = w.save_default()?;
        println!("Wallet imported!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    }
    Ok(())
}

async fn show() -> Result<()> {
    let w = load_wallet_for_show()?;
    let path = if wallet::is_wallet_encrypted() {
        wallet::encrypted_key_path()?
    } else {
        wallet::default_key_path()?
    };
    println!("Address:  {}", w.pubkey());
    println!("Key file: {} {}", path.display(), if wallet::is_wallet_encrypted() { "(encrypted)" } else { "" });
    Ok(())
}

async fn encrypt_wallet() -> Result<()> {
    let json_path = wallet::default_key_path()?;
    if !json_path.exists() {
        if wallet::is_wallet_encrypted() {
            return Err(eyre::eyre!("Wallet is already encrypted."));
        }
        return Err(eyre::eyre!("No wallet found. Run `rwa keys generate` first."));
    }
    let w = Wallet::from_file(&json_path)?;
    let passphrase = prompt_new_passphrase()?;
    let age_path = w.save_default_encrypted(&passphrase)?;
    std::fs::remove_file(&json_path)
        .map_err(|e| eyre::eyre!("Saved encrypted wallet but failed to remove key.json: {e}"))?;
    println!("Wallet encrypted.");
    println!("Key file: {}", age_path.display());
    println!("key.json has been removed.");
    Ok(())
}

async fn decrypt_wallet() -> Result<()> {
    let age_path = wallet::encrypted_key_path()?;
    if !age_path.exists() {
        if wallet::default_key_path()?.exists() {
            return Err(eyre::eyre!("Wallet is not encrypted (key.json found)."));
        }
        return Err(eyre::eyre!("No wallet found."));
    }
    let passphrase = read_passphrase("Enter wallet passphrase: ")?;
    let w = Wallet::from_encrypted_file(&age_path, &passphrase)?;
    let json_path = w.save_default()?;
    std::fs::remove_file(&age_path)
        .map_err(|e| eyre::eyre!("Saved decrypted wallet but failed to remove key.age: {e}"))?;
    println!("Wallet decrypted.");
    println!("Key file: {}", json_path.display());
    println!("key.age has been removed.");
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────

fn load_wallet_for_show() -> Result<Wallet> {
    if wallet::is_wallet_encrypted() {
        let passphrase = read_passphrase_env_or_prompt()?;
        Wallet::load_default_encrypted(&passphrase)
    } else {
        Wallet::load_default()
    }
}

/// Read passphrase from `RWA_PASSPHRASE` env var, otherwise prompt interactively.
fn read_passphrase_env_or_prompt() -> Result<String> {
    if let Ok(p) = std::env::var("RWA_PASSPHRASE") {
        return Ok(p);
    }
    read_passphrase("Wallet passphrase: ")
}

fn read_passphrase(prompt: &str) -> Result<String> {
    rpassword::prompt_password(prompt)
        .map_err(|e| eyre::eyre!("Failed to read passphrase: {e}"))
}

/// Prompt for a new passphrase and ask for confirmation.
fn prompt_new_passphrase() -> Result<String> {
    let p1 = read_passphrase("New passphrase: ")?;
    if p1.is_empty() {
        return Err(eyre::eyre!("Passphrase cannot be empty"));
    }
    let p2 = read_passphrase("Confirm passphrase: ")?;
    if p1 != p2 {
        return Err(eyre::eyre!("Passphrases do not match"));
    }
    Ok(p1)
}
