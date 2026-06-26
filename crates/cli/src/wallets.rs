//! Named-wallet registry: maps user-chosen names to key-file paths.
//!
//! Stored at `<config>/rwa/wallets.toml`. The registry holds *paths*, never key
//! material — key files stay wherever the user put them. An empty/absent
//! registry means "fall back to the legacy single-wallet default", so existing
//! setups keep working untouched.

use eyre::{Result, eyre};
use rwa_ondo::wallet::{self, Wallet};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One registered wallet: a unique name and an absolute path to its key file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalletEntry {
    pub name: String,
    pub path: String,
}

/// The on-disk registry. `active` names the default wallet; `wallets` is the
/// full set. Serialized as TOML with an `[[wallet]]` array-of-tables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WalletRegistry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default, rename = "wallet")]
    pub wallets: Vec<WalletEntry>,
}

/// Path to the registry file under a given config dir: `<config>/rwa/wallets.toml`.
pub fn registry_path(config_dir: &Path) -> PathBuf {
    config_dir.join("rwa").join("wallets.toml")
}

fn legacy_json_path(config_dir: &Path) -> PathBuf {
    config_dir.join("rwa").join("key.json")
}

fn legacy_age_path(config_dir: &Path) -> PathBuf {
    config_dir.join("rwa").join("key.age")
}

impl WalletRegistry {
    /// Load the registry. A missing file is not an error — it yields an empty
    /// registry (the backward-compatible default).
    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = registry_path(config_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| eyre!("Failed to read wallet registry {}: {e}", path.display()))?;
        toml::from_str(&text)
            .map_err(|e| eyre!("Failed to parse wallet registry {}: {e}", path.display()))
    }

    /// Write the registry atomically (temp file + rename) with `0o600` perms.
    /// Atomic rename means a crash mid-write never leaves a half-written file.
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        let dir = config_dir.join("rwa");
        std::fs::create_dir_all(&dir)
            .map_err(|e| eyre!("Failed to create config dir {}: {e}", dir.display()))?;
        let path = registry_path(config_dir);
        let tmp = dir.join("wallets.toml.tmp");
        let text = toml::to_string_pretty(self)
            .map_err(|e| eyre!("Failed to serialize wallet registry: {e}"))?;
        std::fs::write(&tmp, text.as_bytes())
            .map_err(|e| eyre!("Failed to write {}: {e}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| eyre!("Failed to set permissions on {}: {e}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &path)
            .map_err(|e| eyre!("Failed to finalize {}: {e}", path.display()))?;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&WalletEntry> {
        self.wallets.iter().find(|w| w.name == name)
    }

    /// Register a new wallet. Errors if the name is invalid or already taken.
    /// The first wallet added becomes active.
    pub fn add(&mut self, name: &str, path: &str) -> Result<()> {
        validate_name(name)?;
        if self.find(name).is_some() {
            return Err(eyre!("Wallet '{name}' already exists"));
        }
        self.wallets.push(WalletEntry { name: name.to_string(), path: path.to_string() });
        if self.active.is_none() {
            self.active = Some(name.to_string());
        }
        Ok(())
    }

    /// Remove a wallet (the key file is left on disk). If it was active,
    /// `active` is cleared.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        if self.find(name).is_none() {
            return Err(eyre!("Wallet '{name}' not found"));
        }
        self.wallets.retain(|w| w.name != name);
        if self.active.as_deref() == Some(name) {
            self.active = None;
        }
        Ok(())
    }

    /// Set the active wallet. Errors if the name is not registered.
    pub fn set_active(&mut self, name: &str) -> Result<()> {
        if self.find(name).is_none() {
            return Err(eyre!("Wallet '{name}' not found"));
        }
        self.active = Some(name.to_string());
        Ok(())
    }

    /// Comma-separated list of registered names, for error messages.
    pub fn available_names(&self) -> String {
        if self.wallets.is_empty() {
            "(none)".to_string()
        } else {
            self.wallets.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", ")
        }
    }

    /// If any entry points at `old`, repoint it to `new`. Returns true if changed.
    /// Keeps the registry consistent when `keys encrypt`/`decrypt` renames the
    /// legacy key file.
    pub fn repoint_path(&mut self, old: &Path, new: &Path) -> bool {
        let new_s = new.to_string_lossy().to_string();
        let mut changed = false;
        for w in &mut self.wallets {
            if Path::new(&w.path) == old {
                w.path = new_s.clone();
                changed = true;
            }
        }
        changed
    }

    /// Decide which wallet to load given an explicit selection (`--wallet`/env).
    ///
    /// Priority: explicit `selected` > registry `active` > legacy fallback.
    pub fn resolve(&self, selected: Option<&str>) -> Result<WalletTarget> {
        if let Some(name) = selected {
            return match self.find(name) {
                Some(e) => Ok(WalletTarget::Path(PathBuf::from(&e.path))),
                None => Err(eyre!(
                    "Wallet '{name}' not found. Available: {}",
                    self.available_names()
                )),
            };
        }
        if let Some(active) = self.active.as_deref() {
            return match self.find(active) {
                Some(e) => Ok(WalletTarget::Path(PathBuf::from(&e.path))),
                None => Err(eyre!(
                    "Active wallet '{active}' is not registered (inconsistent registry). \
                     Run `rwa keys use <name>`. Available: {}",
                    self.available_names()
                )),
            };
        }
        if self.wallets.is_empty() {
            return Ok(WalletTarget::LegacyDefault);
        }
        Err(eyre!(
            "No active wallet selected. Run `rwa keys use <name>`. Available: {}",
            self.available_names()
        ))
    }
}

/// Validate a wallet name: non-empty, ASCII alphanumerics plus `-`/`_`, max 64.
/// Restrictive on purpose — names appear in CLI flags, JSON, and error text.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(eyre!("Wallet name cannot be empty"));
    }
    if name.len() > 64 {
        return Err(eyre!("Wallet name too long (max 64 characters)"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(eyre!(
            "Invalid wallet name '{name}': use only letters, digits, '-' and '_'"
        ));
    }
    Ok(())
}

/// If the registry is empty but a legacy single-wallet key file exists, register
/// it as `default` and make it active. Called ONLY by registry-management
/// commands (`keys add/list/use`) — never on a trade path, so trades never
/// mutate the registry as a side effect.
pub fn ensure_legacy_registered(config_dir: &Path) -> Result<WalletRegistry> {
    let mut reg = WalletRegistry::load(config_dir)?;
    if !reg.wallets.is_empty() {
        return Ok(reg);
    }
    // Prefer the encrypted legacy file when both exist (mirrors is_wallet_encrypted).
    let legacy = if legacy_age_path(config_dir).exists() {
        Some(legacy_age_path(config_dir))
    } else if legacy_json_path(config_dir).exists() {
        Some(legacy_json_path(config_dir))
    } else {
        None
    };
    if let Some(path) = legacy {
        reg.add("default", &path.to_string_lossy())?;
        reg.save(config_dir)?;
    }
    Ok(reg)
}

/// Load the given target, calling `passphrase` only if the file is encrypted.
/// `passphrase` is a closure so callers (and tests) control how it's obtained.
pub fn load_target(
    target: &WalletTarget,
    passphrase: impl FnOnce() -> Result<String>,
) -> Result<Wallet> {
    match target {
        WalletTarget::Path(path) => {
            if !path.exists() {
                return Err(eyre!(
                    "Wallet key file not found: {}. Re-add it with `rwa keys add`.",
                    path.display()
                ));
            }
            if wallet::is_age_encrypted(path)? {
                let pass = passphrase()?;
                Wallet::from_encrypted_file(path, &pass)
            } else {
                Wallet::from_file(path)
            }
        }
        WalletTarget::LegacyDefault => {
            if wallet::is_wallet_encrypted() {
                let pass = passphrase()?;
                Wallet::load_default_encrypted(&pass)
            } else {
                Wallet::load_default().map_err(|_| {
                    eyre!(
                        "No wallet found.\n\n\
                         Create or import one first:\n  \
                         rwa keys generate                          Create a new wallet\n  \
                         rwa keys import --seed-phrase \"word1 ...\"   Import from seed phrase\n  \
                         rwa keys import --private-key <BASE58>     Import from private key\n  \
                         rwa keys import --file <PATH>              Import from key file"
                    )
                })
            }
        }
    }
}

/// Resolve the active/selected wallet under the real config dir and load it,
/// reading the passphrase from `RWA_PASSPHRASE` or an interactive prompt.
/// Single entry point for trading commands and `keys show`.
pub fn load_selected(selected: Option<&str>) -> Result<Wallet> {
    let config = dirs::config_dir()
        .ok_or_else(|| eyre!("Cannot determine config directory"))?;
    let target = WalletRegistry::load(&config)?.resolve(selected)?;
    load_target(&target, prompt_passphrase)
}

/// Read the wallet passphrase from `RWA_PASSPHRASE` (one-time warning) or prompt.
fn prompt_passphrase() -> Result<String> {
    if let Ok(p) = std::env::var("RWA_PASSPHRASE") {
        warn_passphrase_env_once();
        return Ok(p);
    }
    rpassword::prompt_password("Wallet passphrase: ")
        .map_err(|e| eyre!("Failed to read passphrase: {e}"))
}

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

/// Stable JSON shape for `rwa keys list --json`. `pubkey` is `null` for
/// encrypted wallets (we never prompt for a passphrase just to list).
#[derive(Debug, Serialize)]
pub struct WalletListItemJson {
    pub name: String,
    pub path: String,
    pub pubkey: Option<String>,
    pub active: bool,
    pub encrypted: bool,
}

/// What `resolve` decided to load.
#[derive(Debug, Clone, PartialEq)]
pub enum WalletTarget {
    /// Load this explicit key-file path (a named wallet).
    Path(PathBuf),
    /// No registry involvement — use the legacy single-wallet default location.
    LegacyDefault,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rwa_ondo::wallet::Wallet;

    fn tmp_config() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rwa-wallets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_missing_returns_empty() {
        let cfg = tmp_config();
        let reg = WalletRegistry::load(&cfg).unwrap();
        assert_eq!(reg, WalletRegistry::default());
        assert!(reg.wallets.is_empty());
        assert!(reg.active.is_none());
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let cfg = tmp_config();
        let reg = WalletRegistry {
            active: Some("main".into()),
            wallets: vec![
                WalletEntry { name: "main".into(), path: "/keys/a.json".into() },
                WalletEntry { name: "cold".into(), path: "/keys/b.age".into() },
            ],
        };
        reg.save(&cfg).unwrap();
        let loaded = WalletRegistry::load(&cfg).unwrap();
        assert_eq!(loaded, reg);
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let cfg = tmp_config();
        WalletRegistry::default().save(&cfg).unwrap();
        let meta = std::fs::metadata(registry_path(&cfg)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn validate_name_accepts_safe_names() {
        for n in ["main", "cold-storage", "wallet_2", "A1"] {
            assert!(validate_name(n).is_ok(), "{n} should be valid");
        }
    }

    #[test]
    fn validate_name_rejects_unsafe_names() {
        for n in ["", "has space", "slash/name", "dot.name", "emoji😀"] {
            assert!(validate_name(n).is_err(), "{n} should be rejected");
        }
        assert!(validate_name(&"x".repeat(65)).is_err(), "over 64 chars rejected");
    }

    #[test]
    fn add_first_wallet_becomes_active() {
        let mut reg = WalletRegistry::default();
        reg.add("main", "/k/a.json").unwrap();
        assert_eq!(reg.active.as_deref(), Some("main"));
        reg.add("cold", "/k/b.age").unwrap();
        assert_eq!(reg.active.as_deref(), Some("main"), "active unchanged by 2nd add");
    }

    #[test]
    fn add_duplicate_name_errors() {
        let mut reg = WalletRegistry::default();
        reg.add("main", "/k/a.json").unwrap();
        assert!(reg.add("main", "/k/other.json").is_err());
    }

    #[test]
    fn remove_active_clears_active() {
        let mut reg = WalletRegistry::default();
        reg.add("main", "/k/a.json").unwrap();
        reg.add("cold", "/k/b.age").unwrap();
        reg.remove("main").unwrap();
        assert!(reg.active.is_none());
        assert!(reg.find("main").is_none());
        assert!(reg.find("cold").is_some());
    }

    #[test]
    fn set_active_unknown_errors() {
        let mut reg = WalletRegistry::default();
        assert!(reg.set_active("ghost").is_err());
    }

    fn two_wallet_reg() -> WalletRegistry {
        let mut reg = WalletRegistry::default();
        reg.add("main", "/k/a.json").unwrap();
        reg.add("cold", "/k/b.age").unwrap();
        reg
    }

    #[test]
    fn resolve_explicit_selection() {
        let reg = two_wallet_reg();
        assert_eq!(
            reg.resolve(Some("cold")).unwrap(),
            WalletTarget::Path(PathBuf::from("/k/b.age"))
        );
    }

    #[test]
    fn resolve_unknown_selection_errors() {
        let reg = two_wallet_reg();
        let err = reg.resolve(Some("ghost")).unwrap_err().to_string();
        assert!(err.contains("ghost"), "error mentions the bad name: {err}");
        assert!(err.contains("main"), "error lists available names: {err}");
    }

    #[test]
    fn resolve_falls_back_to_active() {
        let reg = two_wallet_reg(); // active == "main"
        assert_eq!(
            reg.resolve(None).unwrap(),
            WalletTarget::Path(PathBuf::from("/k/a.json"))
        );
    }

    #[test]
    fn resolve_empty_registry_is_legacy_default() {
        let reg = WalletRegistry::default();
        assert_eq!(reg.resolve(None).unwrap(), WalletTarget::LegacyDefault);
    }

    #[test]
    fn resolve_no_active_nonempty_errors() {
        let mut reg = two_wallet_reg();
        reg.remove("main").unwrap(); // clears active, "cold" remains
        let err = reg.resolve(None).unwrap_err().to_string();
        assert!(err.contains("keys use"), "guidance present: {err}");
    }

    fn write_file(p: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn lazy_registers_plaintext_legacy_as_default() {
        let cfg = tmp_config();
        write_file(&cfg.join("rwa").join("key.json"), b"[1,2,3]");
        let reg = ensure_legacy_registered(&cfg).unwrap();
        let e = reg.find("default").expect("default registered");
        assert!(e.path.ends_with("key.json"));
        assert_eq!(reg.active.as_deref(), Some("default"));
        // persisted
        let reloaded = WalletRegistry::load(&cfg).unwrap();
        assert!(reloaded.find("default").is_some());
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn lazy_prefers_encrypted_legacy() {
        let cfg = tmp_config();
        write_file(&cfg.join("rwa").join("key.json"), b"[1,2,3]");
        write_file(&cfg.join("rwa").join("key.age"), b"age-encryption.org/v1\n");
        let reg = ensure_legacy_registered(&cfg).unwrap();
        let e = reg.find("default").unwrap();
        assert!(e.path.ends_with("key.age"), "encrypted legacy preferred");
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn lazy_noop_when_registry_nonempty() {
        let cfg = tmp_config();
        let mut reg = WalletRegistry::default();
        reg.add("main", "/k/a.json").unwrap();
        reg.save(&cfg).unwrap();
        write_file(&cfg.join("rwa").join("key.json"), b"[1,2,3]");
        let out = ensure_legacy_registered(&cfg).unwrap();
        assert!(out.find("default").is_none(), "no default added");
        assert!(out.find("main").is_some());
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn lazy_noop_when_no_legacy_file() {
        let cfg = tmp_config();
        let out = ensure_legacy_registered(&cfg).unwrap();
        assert!(out.wallets.is_empty());
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn repoint_path_updates_matching_entry() {
        let mut reg = WalletRegistry::default();
        reg.add("default", "/cfg/rwa/key.json").unwrap();
        let changed = reg.repoint_path(
            Path::new("/cfg/rwa/key.json"),
            Path::new("/cfg/rwa/key.age"),
        );
        assert!(changed);
        assert_eq!(reg.find("default").unwrap().path, "/cfg/rwa/key.age");
    }

    #[test]
    fn load_target_plaintext_no_passphrase() {
        let cfg = tmp_config();
        let p = cfg.join("k.json");
        let w = Wallet::generate();
        w.save(&p).unwrap();
        let loaded = load_target(
            &WalletTarget::Path(p.clone()),
            || panic!("passphrase must not be requested for plaintext"),
        )
        .unwrap();
        assert_eq!(loaded.pubkey(), w.pubkey());
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn load_target_encrypted_uses_passphrase() {
        let cfg = tmp_config();
        let p = cfg.join("k.age");
        let w = Wallet::generate();
        w.save_encrypted(&p, "TestPass2026!secure").unwrap();
        let loaded = load_target(
            &WalletTarget::Path(p.clone()),
            || Ok("TestPass2026!secure".to_string()),
        )
        .unwrap();
        assert_eq!(loaded.pubkey(), w.pubkey());
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn load_target_missing_path_errors() {
        let res = load_target(
            &WalletTarget::Path(PathBuf::from("/no/such/key.json")),
            || Ok("x".to_string()),
        );
        assert!(res.is_err());
        // Wallet: !Debug, so use .err().unwrap() instead of .unwrap_err()
        assert!(res.err().unwrap().to_string().contains("not found"));
    }

    #[test]
    fn list_item_json_shape() {
        let item = WalletListItemJson {
            name: "cold".into(),
            path: "/k/b.age".into(),
            pubkey: None,
            active: false,
            encrypted: true,
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v.pointer("/name"), Some(&serde_json::Value::from("cold")));
        assert_eq!(v.pointer("/path"), Some(&serde_json::Value::from("/k/b.age")));
        assert_eq!(v.pointer("/pubkey"), Some(&serde_json::Value::Null));
        assert_eq!(v.pointer("/active"), Some(&serde_json::Value::from(false)));
        assert_eq!(v.pointer("/encrypted"), Some(&serde_json::Value::from(true)));
    }
}
