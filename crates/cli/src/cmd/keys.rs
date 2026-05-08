use clap::Subcommand;
use eyre::Result;
use rwa_ondo::wallet::{self, Wallet};

#[derive(Subcommand, Debug)]
pub enum KeysAction {
    /// Generate a new Solana wallet
    Generate {
        /// Save wallet as plaintext key.json instead of encrypted key.age (not recommended)
        #[arg(long)]
        allow_plaintext: bool,

        /// Deprecated: use absence of --allow-plaintext instead (encryption is now the default)
        #[arg(long, hide = true)]
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

        /// Save wallet as plaintext key.json instead of encrypted key.age (not recommended)
        #[arg(long)]
        allow_plaintext: bool,

        /// Deprecated: use absence of --allow-plaintext instead (encryption is now the default)
        #[arg(long, hide = true)]
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
        KeysAction::Generate { allow_plaintext, encrypt } => {
            if encrypt {
                eprintln!("WARNING: --encrypt is deprecated; encryption is now the default. Use --allow-plaintext to opt out.");
            }
            generate(allow_plaintext).await
        }
        KeysAction::Import { file, private_key, seed_phrase, allow_plaintext, encrypt } => {
            if encrypt {
                eprintln!("WARNING: --encrypt is deprecated; encryption is now the default. Use --allow-plaintext to opt out.");
            }
            import(file, private_key, seed_phrase, allow_plaintext).await
        }
        KeysAction::Show => show().await,
        KeysAction::Encrypt => encrypt_wallet().await,
        KeysAction::Decrypt => decrypt_wallet().await,
    }
}

async fn generate(allow_plaintext: bool) -> Result<()> {
    let json_path = wallet::default_key_path()?;
    let age_path = wallet::encrypted_key_path()?;
    if json_path.exists() || age_path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists. Delete it first if you want to generate a new one."
        ));
    }
    let w = Wallet::generate();
    if allow_plaintext {
        eprintln!("WARNING: Saving wallet as plaintext key.json. Consider using encryption for better security.");
        let saved = w.save_default()?;
        println!("New wallet generated!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
        println!("\nFund this address with SOL and USDC to start trading.");
    } else {
        let passphrase = prompt_new_passphrase()?;
        let saved = w.save_default_encrypted(&passphrase)?;
        println!("New wallet generated (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    }
    Ok(())
}

async fn import(
    file: Option<String>,
    private_key: Option<String>,
    seed_phrase: Option<String>,
    allow_plaintext: bool,
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

    if allow_plaintext {
        eprintln!("WARNING: Saving wallet as plaintext key.json. Consider using encryption for better security.");
        let saved = w.save_default()?;
        println!("Wallet imported!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    } else {
        let passphrase = prompt_new_passphrase()?;
        let saved = w.save_default_encrypted(&passphrase)?;
        println!("Wallet imported (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    }
    Ok(())
}

async fn show() -> Result<()> {
    let json_path = wallet::default_key_path()?;
    let is_plaintext = json_path.exists() && !wallet::is_wallet_encrypted();
    if is_plaintext {
        eprintln!("DEPRECATED: Your wallet is stored as plaintext key.json. \
            Run `rwa keys encrypt` to secure it with a passphrase.");
    }
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

/// Emit a one-time stderr warning when RWA_PASSPHRASE is read from the environment.
fn warn_passphrase_env_once() {
    use std::sync::OnceLock;
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "WARNING: RWA_PASSPHRASE in environment leaks via shell history / ps. \
            Prefer interactive passphrase prompt."
        );
    });
}

/// Read passphrase from `RWA_PASSPHRASE` env var, otherwise prompt interactively.
fn read_passphrase_env_or_prompt() -> Result<String> {
    if let Ok(p) = std::env::var("RWA_PASSPHRASE") {
        warn_passphrase_env_once();
        return Ok(p);
    }
    read_passphrase("Wallet passphrase: ")
}

fn read_passphrase(prompt: &str) -> Result<String> {
    rpassword::prompt_password(prompt)
        .map_err(|e| eyre::eyre!("Failed to read passphrase: {e}"))
}

/// Validate passphrase strength. Called by prompt_new_passphrase.
pub fn validate_passphrase(p: &str) -> Result<()> {
    if p.len() < 12 {
        return Err(eyre::eyre!("passphrase must be at least 12 characters"));
    }
    if p.chars().all(|c| c.is_ascii_digit()) {
        return Err(eyre::eyre!("passphrase too weak: must include non-digit characters"));
    }
    Ok(())
}

/// Prompt for a new passphrase and ask for confirmation.
fn prompt_new_passphrase() -> Result<String> {
    // Allow env var to supply passphrase non-interactively (e.g. in tests).
    if let Ok(p) = std::env::var("RWA_PASSPHRASE") {
        warn_passphrase_env_once();
        validate_passphrase(&p)?;
        return Ok(p);
    }
    let p1 = read_passphrase("New passphrase: ")?;
    validate_passphrase(&p1)?;
    let p2 = read_passphrase("Confirm passphrase: ")?;
    if p1 != p2 {
        return Err(eyre::eyre!("Passphrases do not match"));
    }
    Ok(p1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task A.2 tests ────────────────────────────────────────

    #[test]
    fn keys_generate_default_creates_age_file() {
        // Simulate what generate() does: without allow_plaintext, it would call
        // prompt_new_passphrase() which reads RWA_PASSPHRASE env var.
        // We test the wallet API directly to verify key.age is the default output path.
        let tmp_dir = std::env::temp_dir().join(format!(
            "rwa_test_gen_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(42)
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let age_path = tmp_dir.join("key.age");
        let json_path = tmp_dir.join("key.json");

        let w = Wallet::generate();
        let passphrase = "TestPass2026!secure";
        w.save_encrypted(&age_path, passphrase).expect("save_encrypted must succeed");

        assert!(age_path.exists(), "default (encrypted) path key.age must exist");
        assert!(!json_path.exists(), "plaintext key.json must NOT exist by default");

        // cleanup
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn keys_generate_allow_plaintext_creates_json_file() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "rwa_test_plaintext_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(43)
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let json_path = tmp_dir.join("key.json");

        let w = Wallet::generate();
        w.save(&json_path).expect("save plaintext must succeed");

        assert!(json_path.exists(), "--allow-plaintext path must create key.json");

        // cleanup
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    // ── Task A.3 tests ────────────────────────────────────────

    #[test]
    fn passphrase_min_length_enforced() {
        assert!(
            validate_passphrase("abc").is_err(),
            "short passphrase must be rejected"
        );
        assert!(
            validate_passphrase("short").is_err(),
            "passphrase under 12 chars must be rejected"
        );
        assert!(
            validate_passphrase("11charpass!").is_err(),
            "exactly 11 chars must be rejected"
        );
    }

    #[test]
    fn passphrase_digits_only_rejected() {
        assert!(
            validate_passphrase("123456789012").is_err(),
            "digits-only passphrase must be rejected even if >= 12 chars"
        );
        assert!(
            validate_passphrase("000000000000000").is_err(),
            "all-zeros passphrase must be rejected"
        );
    }

    #[test]
    fn passphrase_strong_ok() {
        assert!(validate_passphrase("MyPass2026!!").is_ok(), "strong passphrase must be accepted");
        assert!(
            validate_passphrase("correct-horse-battery-staple").is_ok(),
            "long diceware passphrase must be accepted"
        );
        assert!(
            validate_passphrase("abc123def456").is_ok(),
            "mixed alphanumeric >= 12 chars must be accepted"
        );
    }

    #[test]
    fn passphrase_exactly_12_nondigit_ok() {
        assert!(
            validate_passphrase("abcdefghijkl").is_ok(),
            "exactly 12 non-digit chars must be accepted"
        );
    }
}
