//! Named-wallet registry: maps user-chosen names to key-file paths.
//!
//! Stored at `<config>/rwa/wallets.toml`. The registry holds *paths*, never key
//! material — key files stay wherever the user put them. An empty/absent
//! registry means "fall back to the legacy single-wallet default", so existing
//! setups keep working untouched.

use eyre::{Result, eyre};
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
