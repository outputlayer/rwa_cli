use age::secrecy::Secret;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use eyre::{Result, eyre};
use hmac::{Hmac, Mac};
use sha2::Sha512;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

type HmacSha512 = Hmac<Sha512>;

/// SLIP-10 master key derivation constant for Ed25519.
const SLIP10_ED25519_SEED: &[u8] = b"ed25519 seed";
/// Standard Solana derivation path: m/44'/501'/0'/0'
const SOLANA_DERIVATION_PATH: &[u32] = &[44, 501, 0, 0];

/// A Solana wallet backed by an Ed25519 keypair.
pub struct Wallet {
    signing_key: SigningKey,
}

impl Wallet {
    /// Generate a new random keypair.
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        Self { signing_key }
    }

    /// Load from a solana-keygen compatible JSON file (64-byte array).
    pub fn from_file(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path)?;
        Self::from_json(&data)
    }

    /// Parse from a JSON byte-array string (solana-keygen format).
    fn from_json(json: &str) -> Result<Self> {
        let bytes = Zeroizing::new(serde_json::from_str::<Vec<u8>>(json)?);
        if bytes.len() != 64 {
            return Err(eyre!("Invalid key file: expected 64 bytes, got {}", bytes.len()));
        }
        let secret = Zeroizing::new(bytes[..32].try_into()
            .map_err(|_| eyre!("Failed to extract secret key from file"))?);
        Ok(Self { signing_key: SigningKey::from_bytes(&secret) })
    }

    /// Save wallet encrypted with a passphrase (age format → `key.age`).
    pub fn save_encrypted(&self, path: &Path, passphrase: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut key_bytes = Zeroizing::new(Vec::with_capacity(64));
        key_bytes.extend_from_slice(self.signing_key.as_bytes());
        key_bytes.extend_from_slice(self.verifying_key().as_bytes());
        let json = Zeroizing::new(serde_json::to_string(&*key_bytes)?);

        let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_string()));
        let mut encrypted = vec![];
        let mut writer = encryptor.wrap_output(&mut encrypted)
            .map_err(|e| eyre!("Failed to init encryption: {e}"))?;
        writer.write_all(json.as_bytes())
            .map_err(|e| eyre!("Failed to encrypt wallet: {e}"))?;
        writer.finish()
            .map_err(|e| eyre!("Failed to finalize encryption: {e}"))?;

        std::fs::write(path, &encrypted)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Load wallet from an age-encrypted file.
    pub fn from_encrypted_file(path: &Path, passphrase: &str) -> Result<Self> {
        let encrypted = std::fs::read(path)?;
        let decryptor = match age::Decryptor::new(&encrypted[..])
            .map_err(|e| eyre!("Failed to open encrypted wallet: {e}"))?
        {
            age::Decryptor::Passphrase(d) => d,
            _ => return Err(eyre!("Wallet is not passphrase-encrypted")),
        };
        let mut decrypted = Zeroizing::new(vec![]);
        let mut reader = decryptor
            .decrypt(&Secret::new(passphrase.to_string()), None)
            .map_err(|_| eyre!("Wrong passphrase"))?;
        reader.read_to_end(&mut decrypted)
            .map_err(|e| eyre!("Failed to decrypt wallet: {e}"))?;
        let json = std::str::from_utf8(decrypted.as_slice())
            .map_err(|e| eyre!("Invalid wallet data after decryption: {e}"))?;
        Self::from_json(json)
    }

    /// Save encrypted to the default path `~/.config/rwa/key.age`.
    pub fn save_default_encrypted(&self, passphrase: &str) -> Result<PathBuf> {
        let path = encrypted_key_path()?;
        self.save_encrypted(&path, passphrase)?;
        Ok(path)
    }

    /// Load from the default encrypted path using a passphrase.
    pub fn load_default_encrypted(passphrase: &str) -> Result<Self> {
        let path = encrypted_key_path()?;
        if !path.exists() {
            return Err(eyre!(
                "No encrypted wallet found at {}. \
                 Run `rwa keys generate --encrypt` to create one.",
                path.display()
            ));
        }
        Self::from_encrypted_file(&path, passphrase)
    }

    /// Import from a private key string.
    /// Auto-detects format: JSON byte array, hex, base64, or base58.
    /// Accepts 32-byte (secret only) or 64-byte (keypair) inputs.
    pub fn from_private_key(key_str: &str) -> Result<Self> {
        let trimmed = key_str.trim();
        let bytes = Zeroizing::new(if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // JSON byte array: [1,2,3,...,64]
            serde_json::from_str::<Vec<u8>>(trimmed)
                .map_err(|e| eyre!("Invalid JSON byte array: {e}"))?
        } else if trimmed.len() == 128 || trimmed.len() == 64 {
            // Try hex first (64 or 128 hex chars → 32 or 64 bytes)
            if trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                hex::decode(trimmed)
                    .map_err(|e| eyre!("Invalid hex key: {e}"))?
            } else {
                // Fall through to base58
                bs58::decode(trimmed).into_vec()
                    .map_err(|e| eyre!("Invalid private key: {e}"))?
            }
        } else {
            // Try base64 (44 or 88 chars for 32/64 bytes)
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;
            if let Ok(decoded) = engine.decode(trimmed) {
                if decoded.len() == 32 || decoded.len() == 64 {
                    decoded
                } else {
                    // Not a valid key length for base64, try base58
                    bs58::decode(trimmed).into_vec()
                        .map_err(|e| eyre!("Invalid private key: {e}"))?
                }
            } else {
                // Base58 fallback
                bs58::decode(trimmed).into_vec()
                    .map_err(|e| eyre!("Invalid base58 private key: {e}"))?
            }
        });

        let secret = Zeroizing::new(match bytes.len() {
            64 => bytes[..32].try_into()
                .map_err(|_| eyre!("Failed to extract secret key"))?,
            32 => bytes.as_slice().try_into()
                .map_err(|_| eyre!("Failed to convert to secret key"))?,
            n => return Err(eyre!("Invalid key length: expected 32 or 64 bytes, got {n}")),
        });
        let signing_key = SigningKey::from_bytes(&secret);
        Ok(Self { signing_key })
    }

    /// Import from a BIP39 mnemonic phrase (12 or 24 words).
    /// Derives via SLIP-10 at m/44'/501'/0'/0' (standard Solana path).
    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let mnemonic: bip39::Mnemonic = phrase.parse()
            .map_err(|e| eyre!("Invalid mnemonic: {e}"))?;
        let seed = Zeroizing::new(mnemonic.to_seed(""));

        // SLIP-10 master key generation
        let mut mac = HmacSha512::new_from_slice(SLIP10_ED25519_SEED)
            .map_err(|e| eyre!("HMAC init failed: {e}"))?;
        mac.update(seed.as_slice());
        let result = Zeroizing::new(mac.finalize().into_bytes().to_vec());
        let mut secret = Zeroizing::new(result[..32].to_vec());
        let mut chain_code = Zeroizing::new(result[32..].to_vec());

        // Derive hardened child keys for m/44'/501'/0'/0'
        for &index in SOLANA_DERIVATION_PATH {
            let hardened = index | 0x80000000;
            let mut mac = HmacSha512::new_from_slice(&chain_code)
                .map_err(|e| eyre!("HMAC derive failed: {e}"))?;
            mac.update(&[0x00]);
            mac.update(&secret);
            mac.update(&hardened.to_be_bytes());
            let result = Zeroizing::new(mac.finalize().into_bytes().to_vec());
            secret = Zeroizing::new(result[..32].to_vec());
            chain_code = Zeroizing::new(result[32..].to_vec());
        }

        let secret_bytes = Zeroizing::new(secret.as_slice().try_into()
            .map_err(|_| eyre!("SLIP-10 derivation produced invalid key length"))?);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        Ok(Self { signing_key })
    }

    /// Load the default wallet from ~/.config/rwa/key.json.
    pub fn load_default() -> Result<Self> {
        let path = default_key_path()?;
        if !path.exists() {
            return Err(eyre!(
                "No wallet found. Run `rwa keys generate` first."
            ));
        }
        Self::from_file(&path)
    }

    /// Save wallet to a solana-keygen compatible JSON file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(64));
        bytes.extend_from_slice(self.signing_key.as_bytes());
        bytes.extend_from_slice(self.verifying_key().as_bytes());
        let json = Zeroizing::new(serde_json::to_string(&*bytes)?);
        // Set restrictive permissions before writing
        std::fs::write(path, json.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Save to the default path ~/.config/rwa/key.json.
    pub fn save_default(&self) -> Result<PathBuf> {
        let path = default_key_path()?;
        self.save(&path)?;
        Ok(path)
    }

    /// Base58-encoded public key (Solana address).
    #[must_use]
    pub fn pubkey(&self) -> String {
        bs58::encode(self.verifying_key().as_bytes()).into_string()
    }

    fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Access the signing key for building raw transactions.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Sign a serialized Solana transaction (legacy or versioned).
    ///
    /// Decodes base64, finds the correct signature slot by matching our pubkey
    /// in the transaction's account keys, signs the message, and returns
    /// re-encoded base64.
    pub fn sign_transaction(&self, tx_base64: &str) -> Result<String> {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;

        let mut tx_bytes = engine.decode(tx_base64)?;

        // Parse compact-u16 for number of signatures
        let (num_sigs, sig_count_len) = decode_compact_u16(&tx_bytes)?;
        if num_sigs == 0 {
            return Err(eyre!("Transaction has 0 signature slots"));
        }

        let sigs_start = sig_count_len;
        let sigs_end = sigs_start + (num_sigs as usize) * 64;
        if tx_bytes.len() < sigs_end + 4 {
            return Err(eyre!("Transaction too short"));
        }

        // Message = everything after the signature slots
        let message = &tx_bytes[sigs_end..];

        // Determine version: V0 messages start with 0x80
        let header_offset = if message[0] & 0x80 != 0 { 1 } else { 0 };

        // Message header: [num_required_sigs, num_readonly_signed, num_readonly_unsigned]
        let num_required_sigs = message[header_offset] as usize;
        let keys_compact_start = header_offset + 3;
        let (num_keys, keys_count_len) = decode_compact_u16(&message[keys_compact_start..])?;
        let keys_start = keys_compact_start + keys_count_len;

        // Find our pubkey among the required signers
        let verifying_key = self.signing_key.verifying_key();
        let our_pubkey = verifying_key.as_bytes();
        let search_limit = std::cmp::min(num_keys as usize, num_required_sigs);
        let mut sig_index = None;
        for i in 0..search_limit {
            let offset = keys_start + i * 32;
            if offset + 32 > message.len() {
                break;
            }
            if &message[offset..offset + 32] == our_pubkey.as_slice() {
                sig_index = Some(i);
                break;
            }
        }

        let sig_index = sig_index.ok_or_else(|| {
            eyre!("Wallet pubkey not found in transaction signers — wrong wallet?")
        })?;

        // Sign the message portion
        let signature = self.signing_key.sign(message);

        // Write signature into the correct slot
        let slot_offset = sigs_start + sig_index * 64;
        tx_bytes[slot_offset..slot_offset + 64].copy_from_slice(&signature.to_bytes());

        Ok(engine.encode(&tx_bytes))
    }
}

/// Default path: ~/.config/rwa/key.json
pub fn default_key_path() -> Result<PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| eyre!("Cannot determine config directory"))?;
    Ok(config.join("rwa").join("key.json"))
}

/// Encrypted wallet path: ~/.config/rwa/key.age
pub fn encrypted_key_path() -> Result<PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| eyre!("Cannot determine config directory"))?;
    Ok(config.join("rwa").join("key.age"))
}

/// Returns `true` if an age-encrypted wallet exists.
pub fn is_wallet_encrypted() -> bool {
    encrypted_key_path().map(|p| p.exists()).unwrap_or(false)
}

/// Decode a compact-u16 from the start of a byte slice.
/// Returns (value, bytes_consumed).
fn decode_compact_u16(data: &[u8]) -> Result<(u16, usize)> {
    if data.is_empty() {
        return Err(eyre!("Empty data for compact-u16"));
    }
    let b0 = data[0] as u16;
    if b0 < 0x80 {
        return Ok((b0, 1));
    }
    if data.len() < 2 {
        return Err(eyre!("Truncated compact-u16"));
    }
    let b1 = data[1] as u16;
    if b1 < 0x80 {
        return Ok(((b0 & 0x7f) | (b1 << 7), 2));
    }
    if data.len() < 3 {
        return Err(eyre!("Truncated compact-u16"));
    }
    let b2 = data[2] as u16;
    Ok(((b0 & 0x7f) | ((b1 & 0x7f) << 7) | (b2 << 14), 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_valid_pubkey() {
        let w = Wallet::generate();
        let pk = w.pubkey();
        // Solana pubkeys are 32-44 base58 chars
        assert!(pk.len() >= 32 && pk.len() <= 44);
        // Should decode to 32 bytes
        let bytes = bs58::decode(&pk).into_vec().unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn generate_unique_keys() {
        let w1 = Wallet::generate();
        let w2 = Wallet::generate();
        assert_ne!(w1.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_private_key_roundtrip() {
        let w = Wallet::generate();
        let mut key_bytes = Vec::with_capacity(64);
        key_bytes.extend_from_slice(w.signing_key().as_bytes());
        key_bytes.extend_from_slice(w.signing_key().verifying_key().as_bytes());
        let key_b58 = bs58::encode(&key_bytes).into_string();

        let w2 = Wallet::from_private_key(&key_b58).unwrap();
        assert_eq!(w.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_private_key_32_bytes() {
        let w = Wallet::generate();
        let key_b58 = bs58::encode(w.signing_key().as_bytes()).into_string();

        let w2 = Wallet::from_private_key(&key_b58).unwrap();
        assert_eq!(w.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_private_key_invalid() {
        assert!(Wallet::from_private_key("not-valid-base58!!!").is_err());
    }

    #[test]
    fn from_private_key_hex_64() {
        let w = Wallet::generate();
        let hex_str = hex::encode(w.signing_key().as_bytes());
        assert_eq!(hex_str.len(), 64);
        let w2 = Wallet::from_private_key(&hex_str).unwrap();
        assert_eq!(w.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_private_key_hex_128() {
        let w = Wallet::generate();
        let mut key_bytes = Vec::with_capacity(64);
        key_bytes.extend_from_slice(w.signing_key().as_bytes());
        key_bytes.extend_from_slice(w.signing_key().verifying_key().as_bytes());
        let hex_str = hex::encode(&key_bytes);
        assert_eq!(hex_str.len(), 128);
        let w2 = Wallet::from_private_key(&hex_str).unwrap();
        assert_eq!(w.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_private_key_base64() {
        use base64::Engine;
        let w = Wallet::generate();
        let mut key_bytes = Vec::with_capacity(64);
        key_bytes.extend_from_slice(w.signing_key().as_bytes());
        key_bytes.extend_from_slice(w.signing_key().verifying_key().as_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&key_bytes);
        let w2 = Wallet::from_private_key(&b64).unwrap();
        assert_eq!(w.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_private_key_json_array() {
        let w = Wallet::generate();
        let mut key_bytes = Vec::with_capacity(64);
        key_bytes.extend_from_slice(w.signing_key().as_bytes());
        key_bytes.extend_from_slice(w.signing_key().verifying_key().as_bytes());
        let json = serde_json::to_string(&key_bytes).unwrap();
        let w2 = Wallet::from_private_key(&json).unwrap();
        assert_eq!(w.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_private_key_json_array_32() {
        let w = Wallet::generate();
        let secret: Vec<u8> = w.signing_key().as_bytes().to_vec();
        let json = serde_json::to_string(&secret).unwrap();
        let w2 = Wallet::from_private_key(&json).unwrap();
        assert_eq!(w.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_mnemonic_valid() {
        // Standard 12-word test mnemonic
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let w = Wallet::from_mnemonic(phrase).unwrap();
        let pk = w.pubkey();
        assert!(pk.len() >= 32 && pk.len() <= 44);
    }

    #[test]
    fn from_mnemonic_deterministic() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let w1 = Wallet::from_mnemonic(phrase).unwrap();
        let w2 = Wallet::from_mnemonic(phrase).unwrap();
        assert_eq!(w1.pubkey(), w2.pubkey());
    }

    #[test]
    fn from_mnemonic_invalid() {
        assert!(Wallet::from_mnemonic("not a valid mnemonic").is_err());
    }

    #[test]
    fn sign_transaction_rejects_empty() {
        let w = Wallet::generate();
        // Empty base64 should fail
        assert!(w.sign_transaction("").is_err());
    }

    // Helper: create a unique temp path that cleans itself up on drop.
    struct TempFile(std::path::PathBuf);
    impl TempFile {
        fn new(suffix: &str) -> Self {
            use std::time::{SystemTime, UNIX_EPOCH};
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir()
                .join(format!("rwa_wallet_test_{}_{}.age", ts, suffix));
            TempFile(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let tmp = TempFile::new("roundtrip");
        let passphrase = "correct-horse-battery-staple";

        let original = Wallet::generate();
        original
            .save_encrypted(tmp.path(), passphrase)
            .expect("save_encrypted should succeed");

        let restored = Wallet::from_encrypted_file(tmp.path(), passphrase)
            .expect("from_encrypted_file should succeed with correct passphrase");

        assert_eq!(
            original.pubkey(),
            restored.pubkey(),
            "decrypted wallet must produce the same public key"
        );
        assert_eq!(
            original.signing_key().as_bytes(),
            restored.signing_key().as_bytes(),
            "decrypted wallet must have identical secret key bytes"
        );
    }

    #[test]
    fn decrypt_wrong_passphrase_fails() {
        let tmp = TempFile::new("wrongpass");
        let correct = "right-passphrase";
        let wrong = "wrong-passphrase";

        let wallet = Wallet::generate();
        wallet
            .save_encrypted(tmp.path(), correct)
            .expect("save_encrypted should succeed");

        let result = Wallet::from_encrypted_file(tmp.path(), wrong);
        assert!(
            result.is_err(),
            "decrypting with wrong passphrase must return an error"
        );
    }
}
