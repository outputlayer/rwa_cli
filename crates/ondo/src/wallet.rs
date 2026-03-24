use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use eyre::{Result, eyre};
use hmac::{Hmac, Mac};
use sha2::Sha512;
use std::path::{Path, PathBuf};

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
        let bytes: Vec<u8> = serde_json::from_str(&data)?;
        if bytes.len() != 64 {
            return Err(eyre!("Invalid key file: expected 64 bytes, got {}", bytes.len()));
        }
        let secret: [u8; 32] = bytes[..32].try_into().unwrap();
        let signing_key = SigningKey::from_bytes(&secret);
        Ok(Self { signing_key })
    }

    /// Import from a base58-encoded private key (64-byte keypair or 32-byte secret).
    pub fn from_private_key(key_str: &str) -> Result<Self> {
        let bytes = bs58::decode(key_str).into_vec()
            .map_err(|e| eyre!("Invalid base58 private key: {e}"))?;
        let secret: [u8; 32] = match bytes.len() {
            64 => bytes[..32].try_into().unwrap(),
            32 => bytes.try_into().unwrap(),
            n => return Err(eyre!("Invalid key length: expected 32 or 64 bytes, got {n}")),
        };
        let signing_key = SigningKey::from_bytes(&secret);
        Ok(Self { signing_key })
    }

    /// Import from a BIP39 mnemonic phrase (12 or 24 words).
    /// Derives via SLIP-10 at m/44'/501'/0'/0' (standard Solana path).
    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let mnemonic: bip39::Mnemonic = phrase.parse()
            .map_err(|e| eyre!("Invalid mnemonic: {e}"))?;
        let seed = mnemonic.to_seed("");

        // SLIP-10 master key generation
        let mut mac = HmacSha512::new_from_slice(SLIP10_ED25519_SEED).unwrap();
        mac.update(&seed);
        let result = mac.finalize().into_bytes();
        let mut secret = result[..32].to_vec();
        let mut chain_code = result[32..].to_vec();

        // Derive hardened child keys for m/44'/501'/0'/0'
        for &index in SOLANA_DERIVATION_PATH {
            let hardened = index | 0x80000000;
            let mut mac = HmacSha512::new_from_slice(&chain_code).unwrap();
            mac.update(&[0x00]);
            mac.update(&secret);
            mac.update(&hardened.to_be_bytes());
            let result = mac.finalize().into_bytes();
            secret = result[..32].to_vec();
            chain_code = result[32..].to_vec();
        }

        let secret_bytes: [u8; 32] = secret.try_into().unwrap();
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
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(self.signing_key.as_bytes());
        bytes.extend_from_slice(self.verifying_key().as_bytes());
        let json = serde_json::to_string(&bytes)?;
        // Set restrictive permissions before writing
        std::fs::write(path, &json)?;
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
    pub fn pubkey(&self) -> String {
        bs58::encode(self.verifying_key().as_bytes()).into_string()
    }

    fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Sign a serialized Solana transaction (legacy or versioned).
    ///
    /// Decodes base64, signs the message portion, inserts signature at slot 0,
    /// returns re-encoded base64.
    pub fn sign_transaction(&self, tx_base64: &str) -> Result<String> {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;

        let mut tx_bytes = engine.decode(tx_base64)?;

        // Parse compact-u16 for number of signatures
        let (num_sigs, header_len) = decode_compact_u16(&tx_bytes)?;
        if num_sigs == 0 {
            return Err(eyre!("Transaction has 0 signature slots"));
        }

        let sigs_end = header_len + (num_sigs as usize) * 64;
        if tx_bytes.len() < sigs_end + 1 {
            return Err(eyre!("Transaction too short"));
        }

        // Message = everything after the signature slots
        let message = &tx_bytes[sigs_end..];
        let signature = self.signing_key.sign(message);

        // Write signature into slot 0
        tx_bytes[header_len..header_len + 64].copy_from_slice(&signature.to_bytes());

        Ok(engine.encode(&tx_bytes))
    }
}

/// Default path: ~/.config/rwa/key.json
pub fn default_key_path() -> Result<PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| eyre!("Cannot determine config directory"))?;
    Ok(config.join("rwa").join("key.json"))
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
