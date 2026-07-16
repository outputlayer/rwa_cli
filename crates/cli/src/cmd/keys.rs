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

/// A non-default derivation path only makes sense for a seed-phrase import; with
/// `--file`/`--private-key` it's a likely mistake. Returns `true` when the combo
/// is invalid (non-default path, no seed phrase).
fn derivation_path_misused(derivation_path: &str, has_seed_phrase: bool) -> bool {
    derivation_path != wallet::DEFAULT_DERIVATION_PATH && !has_seed_phrase
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

    /// Store the wallet passphrase in the OS keychain (verified first) so
    /// operational commands run headless without RWA_PASSPHRASE
    StorePassphrase,
    /// Remove the stored passphrase from the OS keychain
    ForgetPassphrase,

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

/// Which passphrase resolution chain (if any) a `keys` subcommand consults —
/// spec A0/A3's two classes (admin / operational), plus "reads no *existing*
/// wallet passphrase at all". Creation-time commands (`generate`/`import`/
/// `encrypt`/`add --seed-phrase`) fall in the third bucket: they read a NEW
/// passphrase via `prompt_new_passphrase`, a deliberately separate,
/// non-TTY-gated flow (see CLAUDE.md's creation-time caveat) — not the
/// `admin_passphrase`/`operational_passphrase` chains this classifier tracks.
/// Test-only: a compile-time tripwire (below), not a runtime dispatch table —
/// see Task 5 / spec A3 gap.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum PassphraseClass {
    /// `admin_passphrase`: a live TTY prompt only, env/keychain deliberately unread.
    Admin,
    /// `operational_passphrase`: `RWA_PASSPHRASE` -> OS keychain -> interactive prompt.
    Operational,
    /// No existing-wallet passphrase is consulted.
    NoPassphrase,
}

/// Compile-time routing tripwire (Task 5 / spec A3 gap): an explicit match
/// over every `KeysAction` variant, not a prose list, so a new variant fails
/// to compile here until someone consciously classifies it.
#[cfg(test)]
fn passphrase_class(action: &KeysAction) -> PassphraseClass {
    use PassphraseClass::{Admin, NoPassphrase, Operational};
    match action {
        KeysAction::StorePassphrase => Admin,
        KeysAction::Decrypt => Admin,
        KeysAction::Export { .. } => Admin,

        KeysAction::Show => Operational,

        KeysAction::Generate { .. }
        | KeysAction::Import { .. }
        | KeysAction::Encrypt
        | KeysAction::ForgetPassphrase
        | KeysAction::Add { .. }
        | KeysAction::List
        | KeysAction::Use { .. }
        | KeysAction::Remove { .. } => NoPassphrase,
    }
}

pub async fn execute(action: KeysAction, json: bool, selected: Option<&str>) -> Result<()> {
    match action {
        KeysAction::Generate { allow_plaintext, encrypt } => {
            if encrypt {
                eprintln!("WARNING: --encrypt is deprecated; encryption is now the default. Use --allow-plaintext to opt out.");
            }
            generate(allow_plaintext, json).await
        }
        KeysAction::Import { file, private_key, seed_phrase, account, derivation_path, allow_plaintext, encrypt } => {
            if encrypt {
                eprintln!("WARNING: --encrypt is deprecated; encryption is now the default. Use --allow-plaintext to opt out.");
            }
            let path = resolve_derivation_path(account, derivation_path)?;
            import(file, private_key, seed_phrase, path, allow_plaintext, json).await
        }
        KeysAction::Show => show(selected, json).await,
        KeysAction::Encrypt => {
            if selected.is_some() {
                eprintln!("NOTE: --wallet/RWA_WALLET is ignored by `keys encrypt`/`decrypt`; operating on the legacy default wallet.");
            }
            encrypt_wallet(json).await
        }
        KeysAction::Decrypt => {
            if selected.is_some() {
                eprintln!("NOTE: --wallet/RWA_WALLET is ignored by `keys encrypt`/`decrypt`; operating on the legacy default wallet.");
            }
            decrypt_wallet(json).await
        }
        KeysAction::StorePassphrase => store_passphrase(selected, json).await,
        KeysAction::ForgetPassphrase => forget_passphrase(selected, json).await,
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

async fn generate(allow_plaintext: bool, json: bool) -> Result<()> {
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
        if json {
            // The mnemonic is included: for plaintext wallets this is the ONLY
            // time it exists anywhere — an agent must be able to capture it.
            println!("{}", wallet_created_json(&w.pubkey(), &saved.display().to_string(), false, Some(&phrase)));
            return Ok(());
        }
        println!("New wallet generated!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
        print_recovery_phrase(&phrase, false);
        println!("\nFund this address with SOL and USDC to start trading.");
    } else {
        let passphrase = prompt_new_passphrase()?;
        let saved = wallet::encrypted_key_path()?;
        w.save_encrypted_with_mnemonic(&saved, &passphrase, Some(&phrase))?;
        if json {
            println!("{}", wallet_created_json(&w.pubkey(), &saved.display().to_string(), true, Some(&phrase)));
            return Ok(());
        }
        println!("New wallet generated (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
        print_recovery_phrase(&phrase, true);
        println!("\nFund this address with SOL and USDC to start trading.");
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
    json: bool,
) -> Result<()> {
    let json_path = wallet::default_key_path()?;
    let age_path = wallet::encrypted_key_path()?;
    if json_path.exists() || age_path.exists() {
        return Err(eyre::eyre!(
            "Wallet already exists. Delete it first if you want to import a new one."
        ));
    }
    if derivation_path_misused(&derivation_path, seed_phrase.is_some()) {
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
        if json {
            // No mnemonic echoed back — the user supplied the secret themselves.
            println!("{}", wallet_created_json(&w.pubkey(), &saved.display().to_string(), false, None));
            return Ok(());
        }
        println!("Wallet imported!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    } else {
        let passphrase = prompt_new_passphrase()?;
        let saved = wallet::encrypted_key_path()?;
        // A seed-phrase import keeps the phrase inside the encrypted payload
        // so `keys export` can reveal it later.
        w.save_encrypted_with_mnemonic(&saved, &passphrase, imported_phrase.as_deref())?;
        if json {
            println!("{}", wallet_created_json(&w.pubkey(), &saved.display().to_string(), true, None));
            return Ok(());
        }
        println!("Wallet imported (encrypted)!");
        println!("Address:  {}", w.pubkey());
        println!("Key file: {}", saved.display());
    }
    Ok(())
}

/// JSON shape for `keys generate`/`keys import` --json: the created wallet's
/// address, key-file path, storage form, and (generate only) the recovery
/// phrase — the same facts the human output prints, machine-readable so an
/// agent can provision a wallet without scraping prose. Pure for unit tests.
fn wallet_created_json(
    pubkey: &str,
    path: &str,
    encrypted: bool,
    mnemonic: Option<&str>,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "status": "ok",
        "pubkey": pubkey,
        "path": path,
        "encrypted": encrypted,
    });
    if let Some(m) = mnemonic {
        v["mnemonic"] = serde_json::Value::String(m.to_string());
    }
    v
}

/// JSON shape for `keys show --json`: the active wallet's address, its key-file
/// path, and whether that file is encrypted. Pure so the shape is unit-testable
/// without touching the filesystem / registry.
fn show_json(pubkey: &str, path: &str, encrypted: bool) -> serde_json::Value {
    serde_json::json!({
        "pubkey": pubkey,
        "path": path,
        "encrypted": encrypted,
    })
}

async fn show(selected: Option<&str>, json: bool) -> Result<()> {
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
    if json {
        println!("{}", serde_json::to_string(&show_json(&w.pubkey(), &path_str, encrypted))?);
        return Ok(());
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
            // Typed → agents get `error_kind: "reveal_required"` and can retry
            // with --reveal instead of parsing prose.
            return Err(rwa_ondo::usecases::gm::GmTradeError::new(
                rwa_ondo::usecases::gm::GmTradeErrorKind::RevealRequired,
                "keys export prints SECRET key material; pass --reveal to confirm (required with --json)",
            )
            .into());
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

    // Admin class (spec A3): secret export takes a typed passphrase only when
    // the wallet is encrypted; plaintext wallets have nothing to prompt for.
    let (w, mnemonic) = crate::wallets::load_target_full(&target, || {
        crate::passphrase::admin_passphrase("keys export --reveal")
    })?;
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
    println!("  {}", *b58);
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

async fn encrypt_wallet(json: bool) -> Result<()> {
    let json_path = wallet::default_key_path()?;
    if !json_path.exists() {
        if wallet::is_wallet_encrypted() {
            return Err(eyre::eyre!("Wallet is already encrypted."));
        }
        return Err(eyre::eyre!("No wallet found. Run `rwa keys generate` first."));
    }
    let w = Wallet::from_file(&json_path)?;
    // `keys encrypt` operates on the plaintext key.json, which never stored the
    // recovery phrase — so the resulting encrypted wallet has NO embedded
    // mnemonic (unlike one created by `keys generate`/seed import). Warn, since
    // `keys export` will then reveal the private key but not the phrase.
    eprintln!(
        "NOTE: the recovery phrase is NOT stored in this encrypted wallet — a plaintext \
         key.json never held it, so encrypting can't add it back. Keep the 12-word phrase \
         you saved at creation; `keys export --reveal` will show the private key but not the phrase."
    );
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
    if json {
        println!(
            "{}",
            serde_json::json!({ "status": "ok", "action": "encrypted", "path": age_path.to_string_lossy(), "encrypted": true })
        );
        return Ok(());
    }
    println!("Wallet encrypted.");
    println!("Key file: {}", age_path.display());
    println!("key.json has been removed.");
    Ok(())
}

async fn decrypt_wallet(json: bool) -> Result<()> {
    let age_path = wallet::encrypted_key_path()?;
    if !age_path.exists() {
        if wallet::default_key_path()?.exists() {
            return Err(eyre::eyre!("Wallet is not encrypted (key.json found)."));
        }
        return Err(eyre::eyre!("No wallet found."));
    }
    // Admin class (spec A3): decrypting strips the at-rest protection, so the
    // passphrase must be typed — RWA_PASSPHRASE/keychain deliberately unread.
    let passphrase = crate::passphrase::admin_passphrase("keys decrypt")?;
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
    if json {
        println!(
            "{}",
            serde_json::json!({ "status": "ok", "action": "decrypted", "path": json_path.to_string_lossy(), "encrypted": false })
        );
        return Ok(());
    }
    println!("Wallet decrypted.");
    println!("Key file: {}", json_path.display());
    println!("key.age has been removed.");
    Ok(())
}

/// Resolve the (name, target) pair the same way `load_selected` does.
fn resolve_wallet_account(selected: Option<&str>) -> Result<(String, crate::wallets::WalletTarget)> {
    let cfg = config_dir()?;
    let reg = crate::wallets::WalletRegistry::load(&cfg)?;
    let account = crate::wallets::keychain_account(&reg, selected);
    let target = reg.resolve(selected)?;
    Ok((account, target))
}

/// Store the active/selected wallet's passphrase in the OS keychain. Only
/// makes sense for encrypted wallets; verifies the passphrase by actually
/// decrypting before touching the keychain, so a typo never gets stored.
async fn store_passphrase(selected: Option<&str>, json: bool) -> Result<()> {
    let (account, target) = resolve_wallet_account(selected)?;
    // Only encrypted wallets have a passphrase to store.
    let encrypted = match &target {
        crate::wallets::WalletTarget::Path(p) => wallet::is_age_encrypted(p)?,
        crate::wallets::WalletTarget::LegacyDefault => wallet::is_wallet_encrypted(),
    };
    if !encrypted {
        return Err(eyre::eyre!(
            "Wallet '{account}' is not encrypted — nothing to store. Encrypt it first: rwa keys encrypt"
        ));
    }
    // Typed by a human, never env/keychain: storing is an admin act.
    let pass = crate::passphrase::admin_passphrase("keys store-passphrase")?;
    // Verify by actually decrypting before touching the keychain.
    crate::wallets::load_target_full(&target, || Ok(pass.clone()))
        .map_err(|e| eyre::eyre!("Passphrase rejected — nothing stored: {e}"))?;
    crate::passphrase::keyring_set(&account, &pass)?;
    if json {
        println!("{}", serde_json::json!({ "status": "ok", "action": "stored", "wallet": account }));
        return Ok(());
    }
    println!("Passphrase for wallet '{account}' stored in the OS keychain.");
    println!("Operational commands now run headless; keys export/decrypt still require typing it.");
    Ok(())
}

/// Remove the wallet's stored passphrase from the OS keychain, if any.
async fn forget_passphrase(selected: Option<&str>, json: bool) -> Result<()> {
    let (account, _) = resolve_wallet_account(selected)?;
    let existed = crate::passphrase::keyring_delete(&account)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "status": "ok", "action": "forgotten", "wallet": account, "existed": existed })
        );
        return Ok(());
    }
    match existed {
        true => println!("Stored passphrase for '{account}' removed from the OS keychain."),
        false => println!("No stored passphrase for '{account}' — nothing to remove."),
    }
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
    if derivation_path_misused(&derivation_path, seed_phrase.is_some()) {
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

    // pubkey when knowable without a passphrase: import mode has the wallet in
    // hand; register-mode plaintext is parsed for validation anyway; a
    // registered age file stays `null` (decrypting just to display would
    // demand the passphrase at registration time).
    let mut added_pubkey: Option<String> = None;
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
            added_pubkey = Some(w.pubkey());
        }
        // Register mode: an existing file must be present and valid.
        None => {
            if !abs.exists() {
                return Err(eyre::eyre!("Key file not found: {}", abs.display()));
            }
            // Validate before registering: parse plaintext fully; for age just confirm header.
            if !wallet::is_age_encrypted(&abs)? {
                added_pubkey = Some(Wallet::from_file(&abs)?.pubkey());
            }
        }
    }

    reg.add(name, &abs.to_string_lossy())?;
    reg.save(&cfg)?;
    let is_active = reg.active.as_deref() == Some(name);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "name": name,
                "path": abs.to_string_lossy(),
                "active": is_active,
                "pubkey": added_pubkey,
                "encrypted": wallet::is_age_encrypted(&abs)?,
            })
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
        .map_err(|e| eyre::eyre!(
            "Failed to read passphrase ({e}). No terminal to prompt? Set RWA_PASSPHRASE, run interactively, or use --allow-plaintext."
        ))
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
    fn account_flag_derives_the_right_wallet_end_to_end() {
        // The full user contract: `--account N` → resolved path → derived wallet.
        const ABANDON: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let path0 = resolve_derivation_path(None, None).unwrap();
        assert_eq!(
            Wallet::from_mnemonic_at(ABANDON, &path0).unwrap().pubkey(),
            "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk"
        );
        // `--account 2` = Phantom/Solflare "Account 3"; independent reference vector.
        let path2 = resolve_derivation_path(Some(2), None).unwrap();
        assert_eq!(
            Wallet::from_mnemonic_at(ABANDON, &path2).unwrap().pubkey(),
            "7WktogJEd2wQ9eH2oWusmcoFTgeYi6rS632UviTBJ2jm"
        );
    }

    #[test]
    fn wallet_created_json_carries_pubkey_path_encrypted_and_optional_mnemonic() {
        // generate --json: agents must get the recovery phrase machine-readably
        // (it is shown exactly once for plaintext wallets).
        let v = wallet_created_json("Pub123", "/x/key.age", true, Some("word1 word2"));
        assert_eq!(v["status"], "ok");
        assert_eq!(v["pubkey"], "Pub123");
        assert_eq!(v["path"], "/x/key.age");
        assert_eq!(v["encrypted"], serde_json::json!(true));
        assert_eq!(v["mnemonic"], "word1 word2");
        // import --json: the user supplied the secret — no mnemonic echoed back.
        let v2 = wallet_created_json("Pub456", "/x/key.json", false, None);
        assert_eq!(v2["encrypted"], serde_json::json!(false));
        assert!(v2.get("mnemonic").is_none(), "no mnemonic key when absent: {v2}");
    }

    #[test]
    fn show_json_emits_pubkey_path_and_encrypted() {
        // `keys show --json` advertises JSON in --help; its shape must carry the
        // same facts the human output shows (address + key file + encryption).
        let v = show_json("HrNd9QEGubs8TnAEmTFZ8QCQJjn2WrcXsYKpqbY4PTci", "/x/key.age", true);
        assert_eq!(v["pubkey"], "HrNd9QEGubs8TnAEmTFZ8QCQJjn2WrcXsYKpqbY4PTci");
        assert_eq!(v["path"], "/x/key.age");
        assert_eq!(v["encrypted"], serde_json::json!(true));
        // plaintext variant flips only `encrypted`
        let v2 = show_json("PubABC", "/x/key.json", false);
        assert_eq!(v2["encrypted"], serde_json::json!(false));
    }

    #[test]
    fn derivation_path_misuse_only_flags_non_default_without_seed() {
        let non_default = "m/44'/501'/1'/0'";
        assert!(derivation_path_misused(non_default, false), "non-default + no seed is a mistake");
        assert!(!derivation_path_misused(non_default, true), "non-default + seed is fine");
        assert!(!derivation_path_misused(wallet::DEFAULT_DERIVATION_PATH, false), "default is always fine");
        assert!(!derivation_path_misused(wallet::DEFAULT_DERIVATION_PATH, true));
    }

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

    // Drives the REAL `generate` command against a hermetic temp config dir, so a
    // regression to plaintext-by-default (a security downgrade) is caught. The
    // old tests bypassed `generate()` entirely — they called `save_encrypted`/
    // `save` on hand-picked paths and asserted the file they just wrote existed,
    // which would pass even if the default flipped to plaintext. (No other test
    // reads `config_dir()` at runtime, so the process-wide HOME override here
    // can't race a sibling; same env-test approach as `wallets.rs`.)
    #[serial_test::serial]
    #[tokio::test]
    #[allow(unsafe_code)] // env::set_var is unsafe on 2024; serialized above so it can't race
    async fn generate_command_defaults_to_encrypted_and_opts_into_plaintext() {
        let base = std::env::temp_dir().join(format!("rwa_gen_cmd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // SAFETY: no other test reads these env vars / config_dir() concurrently
        // (verified: config_dir() is only called by production handlers, none of
        // which other tests invoke), so the process-wide mutation can't race.
        unsafe {
            std::env::set_var("HOME", &base);
            std::env::set_var("XDG_CONFIG_HOME", base.join(".config"));
            std::env::set_var("RWA_PASSPHRASE", "TestPass2026!secure");
        }

        let age = wallet::encrypted_key_path().unwrap();
        let json = wallet::default_key_path().unwrap();
        assert!(!age.exists() && !json.exists(), "clean slate");

        // Default (no --allow-plaintext) MUST produce an encrypted wallet.
        execute(KeysAction::Generate { allow_plaintext: false, encrypt: false }, false, None)
            .await
            .expect("generate (default) should succeed");
        assert!(age.exists(), "default generate must write the encrypted key.age");
        assert!(
            wallet::is_age_encrypted(&age).unwrap(),
            "key.age must be genuinely age-encrypted, not plaintext"
        );
        assert!(!json.exists(), "default generate must NOT write plaintext key.json");

        // Opt in to plaintext on a fresh slate (generate refuses over an existing wallet).
        std::fs::remove_file(&age).unwrap();
        execute(KeysAction::Generate { allow_plaintext: true, encrypt: false }, false, None)
            .await
            .expect("generate (--allow-plaintext) should succeed");
        assert!(json.exists(), "--allow-plaintext must write key.json");
        assert!(!age.exists(), "--allow-plaintext must NOT write key.age");

        let _ = std::fs::remove_dir_all(&base);
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

    // ── Final-review fix wave: Task 5 — command->class routing tripwire ──

    #[test]
    fn keys_action_passphrase_class_routing_matches_spec() {
        // Admin (spec A3): exactly these three variants.
        assert_eq!(passphrase_class(&KeysAction::StorePassphrase), PassphraseClass::Admin);
        assert_eq!(passphrase_class(&KeysAction::Decrypt), PassphraseClass::Admin);
        assert_eq!(passphrase_class(&KeysAction::Export { reveal: true }), PassphraseClass::Admin);

        // Operational: reads via `load_selected` -> `operational_passphrase`.
        assert_eq!(passphrase_class(&KeysAction::Show), PassphraseClass::Operational);

        // Everything else: no *existing*-wallet passphrase is consulted
        // (creation-time flows read a NEW passphrase separately, or no key
        // material is touched at all).
        let no_passphrase_actions = [
            KeysAction::Generate { allow_plaintext: false, encrypt: false },
            KeysAction::Import {
                file: None,
                private_key: None,
                seed_phrase: None,
                account: None,
                derivation_path: None,
                allow_plaintext: false,
                encrypt: false,
            },
            KeysAction::Encrypt,
            KeysAction::ForgetPassphrase,
            KeysAction::Add {
                name: "x".into(),
                path: "x".into(),
                seed_phrase: None,
                account: None,
                derivation_path: None,
                private_key: None,
                allow_plaintext: false,
            },
            KeysAction::List,
            KeysAction::Use { name: "x".into() },
            KeysAction::Remove { name: "x".into() },
        ];
        for action in &no_passphrase_actions {
            assert_eq!(passphrase_class(action), PassphraseClass::NoPassphrase, "{action:?}");
        }
    }
}
