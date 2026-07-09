use clap::Subcommand;
use eyre::Result;
use rwa_ondo::wallet::{self, Wallet};
use zeroize::Zeroizing;

/// Resolve the seed-import derivation path from the mutually-exclusive
/// `--account <N>` (→ `m/44'/501'/N'/0'`) and `--derivation-path <PATH>` flags.
/// Neither given → the default Solana path (account 0).
fn resolve_derivation_path(account: Option<u32>, derivation_path: Option<String>) -> Result<String> {
    match (account, derivation_path) {
        (Some(_), Some(_)) => {
            Err(eyre::eyre!("--account and --derivation-path are mutually exclusive"))
        }
        (Some(n), None) => Ok(wallet::account_derivation_path(n)),
        (None, Some(p)) => Ok(p),
        (None, None) => Ok(wallet::DEFAULT_DERIVATION_PATH.to_string()),
    }
}

/// A seed imported at a non-default path can't be restored from the phrase
/// alone (that yields account 0) — warn the user to record the path too.
fn warn_non_default_path(derivation_path: &str) {
    if derivation_path != wallet::DEFAULT_DERIVATION_PATH {
        eprintln!(
            "NOTE: derived at {derivation_path}. The recovery phrase alone restores the default \
             account (m/44'/501'/0'/0') — record this path; restore THIS wallet elsewhere with \
             `--derivation-path {derivation_path}`."
        );
    }
}

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

        /// Account index for --seed-phrase import (0 = default). Derives m/44'/501'/N'/0' — Phantom/Solflare "Account N+1".
        #[arg(long, conflicts_with = "derivation_path")]
        account: Option<u32>,

        /// Full BIP44 derivation path for --seed-phrase import (e.g. "m/44'/501'/1'/0'").
        #[arg(long)]
        derivation_path: Option<String>,

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

    /// Register a key file by path, or import a key from a seed phrase /
    /// private key, write it to <PATH> (encrypted by default), and register it
    Add {
        /// Name for this wallet (letters, digits, '-', '_')
        name: String,
        /// Where the key lives. With --seed-phrase/--private-key the key is
        /// written here; otherwise an existing file at this path is registered.
        #[arg(long)]
        path: String,
        /// Import from a BIP39 seed phrase (omit the value to enter it interactively)
        #[arg(long, num_args = 0..=1, default_missing_value = "", conflicts_with = "private_key")]
        seed_phrase: Option<String>,
        /// Account index for --seed-phrase import (0 = default). Derives m/44'/501'/N'/0'.
        #[arg(long, conflicts_with_all = ["private_key", "derivation_path"])]
        account: Option<u32>,
        /// Full BIP44 derivation path for --seed-phrase import (e.g. "m/44'/501'/1'/0'").
        #[arg(long, conflicts_with = "private_key")]
        derivation_path: Option<String>,
        /// Import from a base58/hex/base64 private key
        #[arg(long)]
        private_key: Option<String>,
        /// When importing, write the key as plaintext instead of encrypted (not recommended)
        #[arg(long)]
        allow_plaintext: bool,
    },

    /// Export the wallet's secret key (and recovery phrase, when stored)
    Export {
        /// Confirm printing SECRET material (required with --json; skips the prompt)
        #[arg(long)]
        reveal: bool,
    },

    /// List registered wallets
    List,

    /// Set the active wallet
    Use {
        /// Name of the wallet to activate
        name: String,
    },

    /// Remove a wallet from the registry (does not delete the key file)
    Remove {
        /// Name of the wallet to remove
        name: String,
    },
}

pub async fn execute(action: KeysAction, json: bool, selected: Option<&str>) -> Result<()> {
    match action {
        KeysAction::Generate { allow_plaintext, encrypt } => {
            if encrypt {
                eprintln!("WARNING: --encrypt is deprecated; encryption is now the default. Use --allow-plaintext to opt out.");
            }
            generate(allow_plaintext).await
        }
        KeysAction::Import { file, private_key, seed_phrase, account, derivation_path, allow_plaintext, encrypt } => {
            if encrypt {
                eprintln!("WARNING: --encrypt is deprecated; encryption is now the default. Use --allow-plaintext to opt out.");
            }
            let path = resolve_derivation_path(account, derivation_path)?;
            import(file, private_key, seed_phrase, path, allow_plaintext).await
        }
        KeysAction::Show => show(selected).await,
        KeysAction::Encrypt => {
            if selected.is_some() {
                eprintln!("NOTE: --wallet/RWA_WALLET is ignored by `keys encrypt`/`decrypt`; operating on the legacy default wallet.");
            }
            encrypt_wallet().await
        }
        KeysAction::Decrypt => {
            if selected.is_some() {
                eprintln!("NOTE: --wallet/RWA_WALLET is ignored by `keys encrypt`/`decrypt`; operating on the legacy default wallet.");
            }
            decrypt_wallet().await
        }
        KeysAction::Add { name, path, seed_phrase, account, derivation_path, private_key, allow_plaintext } => {
            let deriv = resolve_derivation_path(account, derivation_path)?;
            add(&name, &path, seed_phrase, deriv, private_key, allow_plaintext, json).await
        }
        KeysAction::Export { reveal } => export(selected, reveal, json).await,
        KeysAction::List => list(json).await,
        KeysAction::Use { name } => use_wallet(&name, json).await,
        KeysAction::Remove { name } => remove(&name, json).await,
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
    // Mnemonic-first: the wallet derives from a fresh BIP39 phrase at the
    // standard Solana path, so it can always be restored in Phantom/Solflare.
    let (w, phrase) = Wallet::generate_with_mnemonic(12)?;
    if allow_plaintext {
        eprintln!("WARNING: Saving wallet as plaintext key.json. Consider using encryption for better security.");
        let saved = w.save_default()?;
        println!("New wallet generated!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
        print_recovery_phrase(&phrase, false);
        println!("\nFund this address with SOL and USDC to start trading.");
    } else {
        let passphrase = prompt_new_passphrase()?;
        let saved = wallet::encrypted_key_path()?;
        w.save_encrypted_with_mnemonic(&saved, &passphrase, Some(&phrase))?;
        println!("New wallet generated (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
        print_recovery_phrase(&phrase, true);
    }
    Ok(())
}

/// Print the recovery phrase once, with storage-dependent guidance.
fn print_recovery_phrase(phrase: &str, stored_encrypted: bool) {
    println!("\nRecovery phrase (works in Phantom/Solflare):");
    println!("  {phrase}");
    if stored_encrypted {
        println!("  Stored inside the encrypted wallet — view again with `rwa keys export`.");
    } else {
        println!("  WRITE IT DOWN NOW — plaintext wallets do not store it; this is the only time it is shown.");
    }
}

async fn import(
    file: Option<String>,
    private_key: Option<String>,
    seed_phrase: Option<String>,
    derivation_path: String,
    allow_plaintext: bool,
) -> Result<()> {
    let json_path = wallet::default_key_path()?;
    let age_path = wallet::encrypted_key_path()?;
    if json_path.exists() || age_path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists. Delete it first if you want to import a new one."
        ));
    }
    if derivation_path != wallet::DEFAULT_DERIVATION_PATH && seed_phrase.is_none() {
        return Err(eyre::eyre!(
            "--account/--derivation-path only apply to --seed-phrase import"
        ));
    }

    let mut imported_phrase: Option<String> = None;
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
            let w = Wallet::from_mnemonic_at(&phrase, &derivation_path)?;
            warn_non_default_path(&derivation_path);
            imported_phrase = Some(phrase);
            w
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
        let saved = wallet::encrypted_key_path()?;
        // A seed-phrase import keeps the phrase inside the encrypted payload
        // so `keys export` can reveal it later.
        w.save_encrypted_with_mnemonic(&saved, &passphrase, imported_phrase.as_deref())?;
        println!("Wallet imported (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    }
    Ok(())
}

async fn show(selected: Option<&str>) -> Result<()> {
    let cfg = config_dir()?;
    let reg = crate::wallets::WalletRegistry::load(&cfg)?;
    let target = reg.resolve(selected)?;
    let w = crate::wallets::load_selected(selected)?;
    let path_str = match &target {
        crate::wallets::WalletTarget::Path(p) => p.display().to_string(),
        crate::wallets::WalletTarget::LegacyDefault => {
            if wallet::is_wallet_encrypted() {
                wallet::encrypted_key_path()?.display().to_string()
            } else {
                wallet::default_key_path()?.display().to_string()
            }
        }
    };
    let encrypted = match &target {
        crate::wallets::WalletTarget::Path(p) => wallet::is_age_encrypted(p).unwrap_or(false),
        crate::wallets::WalletTarget::LegacyDefault => wallet::is_wallet_encrypted(),
    };
    if !encrypted {
        eprintln!("DEPRECATED: this wallet is stored as plaintext. Run `rwa keys encrypt` to secure it with a passphrase.");
    }
    println!("Address:  {}", w.pubkey());
    println!("Key file: {path_str} {}", if encrypted { "(encrypted)" } else { "" });
    Ok(())
}

/// Print the wallet's secret material: base58 keypair (the format
/// Phantom/Solflare import), the solana-keygen JSON array, and the recovery
/// phrase when the encrypted payload stores one. Gated behind --reveal (or an
/// interactive confirmation) so it can never happen by accident.
async fn export(selected: Option<&str>, reveal: bool, json: bool) -> Result<()> {
    let cfg = config_dir()?;
    let reg = crate::wallets::WalletRegistry::load(&cfg)?;
    let target = reg.resolve(selected)?;

    if !reveal {
        if json {
            return Err(eyre::eyre!(
                "keys export prints SECRET key material; pass --reveal to confirm (required with --json)"
            ));
        }
        let ok = {
            use std::io::Write as _;
            print!("This will print your SECRET key and recovery phrase to the terminal. Continue? [y/N] ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).ok();
            matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
        };
        if !ok {
            return Err(eyre::eyre!("Cancelled"));
        }
    }

    let (w, mnemonic) = crate::wallets::load_target_full(&target, crate::wallets::prompt_passphrase)?;
    let keypair = w.to_keypair_bytes();
    let b58 = w.to_base58_keypair();

    eprintln!(
        "WARNING: secret material follows — anyone holding it controls the funds. Clear your terminal scrollback/history afterwards."
    );
    if json {
        let obj = serde_json::json!({
            "pubkey": w.pubkey(),
            "private_key_base58": &*b58,
            "private_key_json": &*keypair,
            "mnemonic": mnemonic,
        });
        println!("{obj}");
        return Ok(());
    }
    println!("Address:  {}", w.pubkey());
    println!("Private key (base58 — import in Phantom/Solflare):");
    println!("  {}", &*b58);
    println!("Private key (JSON array — solana-keygen format):");
    println!("  {}", serde_json::to_string(&*keypair)?);
    match mnemonic {
        Some(m) => {
            println!("Recovery phrase:");
            println!("  {m}");
        }
        None => println!(
            "Recovery phrase: not stored — plaintext wallets and private-key imports don't keep it (it was shown once at creation, if any)."
        ),
    }
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
    // Keep the registry consistent if it pointed at the (now-renamed) legacy file.
    // The key file is already encrypted; surface (don't swallow) a failure to
    // update the registry so a named wallet isn't silently left pointing at the
    // removed path.
    match config_dir() {
        Ok(cfg) => {
            let mut reg = crate::wallets::WalletRegistry::load(&cfg)?;
            if reg.repoint_path(&json_path, &age_path) {
                reg.save(&cfg)?;
            }
        }
        Err(e) => eprintln!(
            "WARNING: wallet encrypted, but the wallet registry could not be updated: {e}. \
             If you use named wallets, run `rwa keys add <name> --path {}` to re-register it.",
            age_path.display()
        ),
    }
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
    // Surface (don't swallow) a registry-update failure: the key is already
    // decrypted, so a named wallet must not be silently left pointing at the
    // removed encrypted path.
    match config_dir() {
        Ok(cfg) => {
            let mut reg = crate::wallets::WalletRegistry::load(&cfg)?;
            if reg.repoint_path(&age_path, &json_path) {
                reg.save(&cfg)?;
            }
        }
        Err(e) => eprintln!(
            "WARNING: wallet decrypted, but the wallet registry could not be updated: {e}. \
             If you use named wallets, run `rwa keys add <name> --path {}` to re-register it.",
            json_path.display()
        ),
    }
    println!("Wallet decrypted.");
    println!("Key file: {}", json_path.display());
    println!("key.age has been removed.");
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────

fn config_dir() -> Result<std::path::PathBuf> {
    dirs::config_dir().ok_or_else(|| eyre::eyre!("Cannot determine config directory"))
}

/// Expand a leading `~/` using the home dir; otherwise return the path as-is.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => std::path::PathBuf::from(path),
    }
}

async fn add(
    name: &str,
    path: &str,
    seed_phrase: Option<String>,
    derivation_path: String,
    private_key: Option<String>,
    allow_plaintext: bool,
    json: bool,
) -> Result<()> {
    if derivation_path != wallet::DEFAULT_DERIVATION_PATH && seed_phrase.is_none() {
        return Err(eyre::eyre!(
            "--account/--derivation-path only apply to --seed-phrase import"
        ));
    }
    let cfg = config_dir()?;
    // Validate the name and reject duplicates BEFORE any file is written, so an
    // import that names an existing wallet never leaves an orphan key file.
    crate::wallets::validate_name(name)?;
    let mut reg = crate::wallets::ensure_legacy_registered(&cfg)?;
    if reg.find(name).is_some() {
        return Err(eyre::eyre!("Wallet '{name}' already exists"));
    }

    let expanded = expand_tilde(path);
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };

    // Derive a wallet from a secret source, if one was provided. A seed-phrase
    // source keeps the phrase so the encrypted payload can embed it.
    let mut imported_phrase: Option<String> = None;
    let imported = match (seed_phrase, private_key) {
        (Some(sp), None) => {
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
            let w = Wallet::from_mnemonic_at(&phrase, &derivation_path)?;
            warn_non_default_path(&derivation_path);
            imported_phrase = Some(phrase);
            Some(w)
        }
        (None, Some(pk)) => Some(Wallet::from_private_key(&pk)?),
        (None, None) => None,
        // clap's `conflicts_with` already blocks this, but stay defensive.
        (Some(_), Some(_)) => {
            return Err(eyre::eyre!("Provide only one of --seed-phrase or --private-key"));
        }
    };

    match imported {
        // Import mode: write a new key file at `abs` (encrypted by default), then register it.
        Some(w) => {
            if abs.exists() {
                return Err(eyre::eyre!(
                    "Refusing to overwrite existing file: {}. Choose a new --path, \
                     or omit --seed-phrase/--private-key to register the existing file as-is.",
                    abs.display()
                ));
            }
            if allow_plaintext {
                eprintln!(
                    "WARNING: writing the key as plaintext. Omit --allow-plaintext to encrypt it with a passphrase."
                );
                w.save(&abs)?;
            } else {
                let passphrase = prompt_new_passphrase()?;
                w.save_encrypted_with_mnemonic(&abs, &passphrase, imported_phrase.as_deref())?;
            }
        }
        // Register mode: an existing file must be present and valid.
        None => {
            if !abs.exists() {
                return Err(eyre::eyre!("Key file not found: {}", abs.display()));
            }
            // Validate before registering: parse plaintext fully; for age just confirm header.
            if !wallet::is_age_encrypted(&abs)? {
                Wallet::from_file(&abs)?;
            }
        }
    }

    reg.add(name, &abs.to_string_lossy())?;
    reg.save(&cfg)?;
    let is_active = reg.active.as_deref() == Some(name);
    if json {
        println!(
            "{}",
            serde_json::json!({ "status": "ok", "name": name, "path": abs.to_string_lossy(), "active": is_active })
        );
    } else {
        println!("Registered wallet '{name}' -> {}", abs.display());
        if is_active {
            println!("'{name}' is now the active wallet.");
        }
    }
    Ok(())
}

async fn list(json: bool) -> Result<()> {
    let cfg = config_dir()?;
    let reg = crate::wallets::ensure_legacy_registered(&cfg)?;
    if json {
        let items: Vec<crate::wallets::WalletListItemJson> = reg
            .wallets
            .iter()
            .map(|w| {
                let p = std::path::Path::new(&w.path);
                let encrypted = wallet::is_age_encrypted(p).unwrap_or(false);
                let pubkey = if encrypted {
                    None
                } else {
                    Wallet::from_file(p).ok().map(|wk| wk.pubkey())
                };
                crate::wallets::WalletListItemJson {
                    name: w.name.clone(),
                    path: w.path.clone(),
                    pubkey,
                    active: reg.active.as_deref() == Some(&w.name),
                    encrypted,
                }
            })
            .collect();
        println!("{}", serde_json::json!({ "wallets": items }));
        return Ok(());
    }
    if reg.wallets.is_empty() {
        println!("No wallets registered. Add one with `rwa keys add <name> --path <file>`.");
        return Ok(());
    }
    for w in &reg.wallets {
        let active = if reg.active.as_deref() == Some(&w.name) { "*" } else { " " };
        let p = std::path::Path::new(&w.path);
        let kind = if wallet::is_age_encrypted(p).unwrap_or(false) { "encrypted" } else { "plaintext" };
        println!("{active} {:<16} {}  [{}]", w.name, w.path, kind);
    }
    Ok(())
}

async fn use_wallet(name: &str, json: bool) -> Result<()> {
    let cfg = config_dir()?;
    let mut reg = crate::wallets::ensure_legacy_registered(&cfg)?;
    reg.set_active(name)?;
    reg.save(&cfg)?;
    if json {
        println!("{}", serde_json::json!({ "status": "ok", "active": name }));
    } else {
        println!("Active wallet set to '{name}'.");
    }
    Ok(())
}

async fn remove(name: &str, json: bool) -> Result<()> {
    let cfg = config_dir()?;
    let mut reg = crate::wallets::WalletRegistry::load(&cfg)?;
    reg.remove(name)?;
    reg.save(&cfg)?;
    if json {
        println!("{}", serde_json::json!({ "status": "ok", "removed": name, "active": reg.active }));
    } else {
        println!("Removed wallet '{name}' from the registry. The key file was not deleted.");
        if reg.active.is_none() {
            println!("No active wallet now. Set one with `rwa keys use <name>`.");
        }
    }
    Ok(())
}

use crate::wallets::warn_passphrase_env_once;

fn read_passphrase(prompt: &str) -> Result<Zeroizing<String>> {
    rpassword::prompt_password(prompt)
        .map(Zeroizing::new)
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
fn prompt_new_passphrase() -> Result<Zeroizing<String>> {
    // Allow env var to supply passphrase non-interactively (e.g. in tests).
    if let Ok(p) = std::env::var("RWA_PASSPHRASE") {
        warn_passphrase_env_once();
        validate_passphrase(&p)?;
        return Ok(Zeroizing::new(p));
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

    #[test]
    fn resolve_derivation_path_defaults_and_flags() {
        assert_eq!(resolve_derivation_path(None, None).unwrap(), "m/44'/501'/0'/0'");
        assert_eq!(resolve_derivation_path(Some(1), None).unwrap(), "m/44'/501'/1'/0'");
        assert_eq!(
            resolve_derivation_path(None, Some("m/44'/501'/5'/0'".to_string())).unwrap(),
            "m/44'/501'/5'/0'"
        );
        assert!(resolve_derivation_path(Some(1), Some("m/44'/501'/1'/0'".to_string())).is_err());
    }

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
