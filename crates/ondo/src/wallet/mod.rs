pub(crate) mod parser;
pub mod verify;

pub use verify::{decode_and_verify, ExpectedSwap, VerifyError};

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

/// Default Solana derivation path — Phantom/Solflare "Account 1" (index 0).
pub const DEFAULT_DERIVATION_PATH: &str = "m/44'/501'/0'/0'";

/// Standard Solana derivation path for account `n`: `m/44'/501'/n'/0'`.
/// `account_derivation_path(0)` is the default; `n` maps to the (n+1)-th
/// account label in Phantom/Solflare.
pub fn account_derivation_path(n: u32) -> String {
    format!("m/44'/501'/{n}'/0'")
}

/// Parse a BIP-44 path like `m/44'/501'/1'/0'` into raw indices. The `'`/`h`
/// hardened markers are accepted; ed25519/SLIP-10 hardens every level during
/// derivation regardless, so the markers are advisory.
fn parse_derivation_path(path: &str) -> Result<Vec<u32>> {
    let mut segments = path.trim().split('/');
    match segments.next() {
        Some("m") | Some("M") => {}
        _ => return Err(eyre!("derivation path must start with 'm', got {path:?}")),
    }
    let mut indices = Vec::new();
    for seg in segments {
        let seg = seg.trim();
        if seg.is_empty() {
            continue; // tolerate a trailing slash
        }
        let num = seg
            .strip_suffix('\'')
            .or_else(|| seg.strip_suffix('h'))
            .unwrap_or(seg);
        let idx: u32 = num
            .parse()
            .map_err(|_| eyre!("invalid derivation path segment {seg:?} in {path:?}"))?;
        if idx >= 0x8000_0000 {
            return Err(eyre!("derivation index {idx} out of range in {path:?}"));
        }
        indices.push(idx);
    }
    if indices.is_empty() {
        return Err(eyre!("derivation path {path:?} has no indices"));
    }
    Ok(indices)
}

/// A Solana wallet backed by an Ed25519 keypair.
pub struct Wallet {
    signing_key: SigningKey,
}

/// A single recipient address allow-listed for `send`, stored inside the
/// encrypted wallet payload (v3).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AllowedRecipient {
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub added_at: String, // RFC3339
}

/// Send-time allow list, embedded (encrypted) alongside the key and mnemonic
/// in the payload v3 age container.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SendPolicy {
    #[serde(default)]
    pub allowed_recipients: Vec<AllowedRecipient>,
}

impl SendPolicy {
    /// True when `addr` is in the allowed list (exact base58 match).
    #[must_use]
    pub fn permits(&self, addr: &str) -> bool {
        self.allowed_recipients.iter().any(|r| r.address == addr)
    }

    /// Add an address. Errors on exact-duplicate address.
    pub fn allow(&mut self, address: &str, label: Option<&str>, added_at: &str) -> Result<()> {
        if self.permits(address) {
            return Err(eyre!("{address} is already in the allowed-recipients list"));
        }
        self.allowed_recipients.push(AllowedRecipient {
            address: address.to_string(),
            label: label.map(str::to_string),
            added_at: added_at.to_string(),
        });
        Ok(())
    }

    /// Remove an address; returns whether it was present.
    pub fn remove(&mut self, address: &str) -> bool {
        let before = self.allowed_recipients.len();
        self.allowed_recipients.retain(|r| r.address != address);
        self.allowed_recipients.len() != before
    }
}

/// Result of decrypting a payload v1/v2/v3 age container: the wallet plus
/// whatever optional extras the payload carried.
pub struct DecryptedWallet {
    pub wallet: Wallet,
    // Audit L3: the recovery mnemonic alone drains the wallet (it re-derives
    // the signing key), so it's held `Zeroizing` like every other secret here
    // — zeroed on drop instead of lingering in freed heap/swap/core dumps.
    pub mnemonic: Option<Zeroizing<String>>,
    pub policy: Option<SendPolicy>,
}

impl Wallet {
    /// Generate a new random keypair.
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        Self { signing_key }
    }

    /// Generate a fresh wallet from a new BIP39 mnemonic (12 or 24 words),
    /// derived at the standard Solana path — the same wallet Phantom/Solflare
    /// would restore from the phrase. Returns the wallet and the phrase,
    /// `Zeroizing` since it alone can re-derive the signing key (audit L3).
    pub fn generate_with_mnemonic(word_count: usize) -> Result<(Self, Zeroizing<String>)> {
        use rand::RngCore;
        let entropy_len = if word_count == 24 { 32 } else { 16 };
        let mut entropy = Zeroizing::new(vec![0u8; entropy_len]);
        rand::thread_rng().fill_bytes(&mut entropy);
        let mnemonic = bip39::Mnemonic::from_entropy(&entropy)
            .map_err(|e| eyre!("mnemonic generation failed: {e}"))?;
        let phrase = Zeroizing::new(mnemonic.to_string());
        let wallet = Self::from_mnemonic(&phrase)?;
        Ok((wallet, phrase))
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
        self.save_encrypted_with_mnemonic(path, passphrase, None)
    }

    /// Save encrypted, optionally embedding the recovery mnemonic inside the
    /// encrypted payload (object `{"key": [...], "mnemonic": "..."}` instead
    /// of the legacy bare array) so `keys export` can reveal it later. The
    /// mnemonic is NEVER written outside the age container.
    ///
    /// Thin wrapper over [`Self::save_encrypted_payload`] with `policy: None`
    /// — signature preserved for existing callers.
    pub fn save_encrypted_with_mnemonic(
        &self,
        path: &Path,
        passphrase: &str,
        mnemonic: Option<&str>,
    ) -> Result<()> {
        self.save_encrypted_payload(path, passphrase, mnemonic, None)
    }

    /// Save encrypted, optionally embedding the recovery mnemonic and/or a
    /// [`SendPolicy`] inside the encrypted payload. Always writes the object
    /// form when either extra is present (`{"key": [...], "mnemonic": "...",
    /// "policy": {...}}`), omitting whichever key(s) are `None`; with both
    /// `None` this writes the legacy bare-array form (identical to
    /// `save_encrypted`). The mnemonic and policy are NEVER written outside
    /// the age container.
    pub fn save_encrypted_payload(
        &self,
        path: &Path,
        passphrase: &str,
        mnemonic: Option<&str>,
        policy: Option<&SendPolicy>,
    ) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut key_bytes = Zeroizing::new(Vec::with_capacity(64));
        key_bytes.extend_from_slice(self.signing_key.as_bytes());
        key_bytes.extend_from_slice(self.verifying_key().as_bytes());
        let json = Zeroizing::new(if mnemonic.is_some() || policy.is_some() {
            let mut obj = serde_json::Map::new();
            obj.insert("key".to_string(), serde_json::to_value(&*key_bytes)?);
            if let Some(phrase) = mnemonic {
                obj.insert("mnemonic".to_string(), serde_json::Value::String(phrase.to_string()));
            }
            if let Some(p) = policy {
                obj.insert("policy".to_string(), serde_json::to_value(p)?);
            }
            serde_json::to_string(&serde_json::Value::Object(obj))?
        } else {
            serde_json::to_string(&*key_bytes)?
        });

        let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_string()));
        let mut encrypted = vec![];
        let mut writer = encryptor.wrap_output(&mut encrypted)
            .map_err(|e| eyre!("Failed to init encryption: {e}"))?;
        writer.write_all(json.as_bytes())
            .map_err(|e| eyre!("Failed to encrypt wallet: {e}"))?;
        writer.finish()
            .map_err(|e| eyre!("Failed to finalize encryption: {e}"))?;

        write_key_file(path, &encrypted)
    }

    /// Load wallet from an age-encrypted file.
    pub fn from_encrypted_file(path: &Path, passphrase: &str) -> Result<Self> {
        Ok(Self::from_encrypted_file_full(path, passphrase)?.0)
    }

    /// Load an encrypted wallet plus its stored recovery mnemonic, when the
    /// payload carries one. Accepts the legacy bare-array payload and both
    /// object forms (v2 `{key, mnemonic}`, v3 `{key, mnemonic, policy}`).
    ///
    /// Thin wrapper over [`Self::load_encrypted_payload`], dropping the
    /// policy — signature preserved for existing callers.
    ///
    /// Audit L3: this public boundary still returns a plain `String` (kept
    /// as-is rather than rippling `Zeroizing` through the several existing
    /// callers of this exact signature). Every hop up to here — the
    /// deserialize buffer, the `Payload.mnemonic` field, and
    /// [`DecryptedWallet.mnemonic`] — stays `Zeroizing`; only this final
    /// handoff converts, so callers that only display or discard the phrase
    /// don't need to change.
    pub fn from_encrypted_file_full(
        path: &Path,
        passphrase: &str,
    ) -> Result<(Self, Option<String>)> {
        let decrypted = Self::load_encrypted_payload(path, passphrase)?;
        Ok((decrypted.wallet, decrypted.mnemonic.map(|z| z.to_string())))
    }

    /// Load an encrypted wallet plus whatever optional extras its payload
    /// carries: v1 (bare key array) → mnemonic `None`, policy `None`; v2
    /// `{key, mnemonic}` → policy `None`; v3 `{key, mnemonic, policy}` → full.
    pub fn load_encrypted_payload(path: &Path, passphrase: &str) -> Result<DecryptedWallet> {
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
        if json.trim_start().starts_with('{') {
            #[derive(serde::Deserialize)]
            struct Payload {
                key: Vec<u8>,
                // Audit L3: deserialize straight into `Zeroizing` (zeroize's
                // `serde` feature) so the phrase never sits as a bare String
                // in this local before being moved into `DecryptedWallet`.
                #[serde(default)]
                mnemonic: Option<Zeroizing<String>>,
                #[serde(default)]
                policy: Option<SendPolicy>,
            }
            let payload: Payload = serde_json::from_str(json)
                .map_err(|e| eyre!("Invalid wallet payload after decryption: {e}"))?;
            let key_json = Zeroizing::new(serde_json::to_string(&payload.key)?);
            let wallet = Self::from_json(&key_json)?;
            return Ok(DecryptedWallet { wallet, mnemonic: payload.mnemonic, policy: payload.policy });
        }
        Ok(DecryptedWallet { wallet: Self::from_json(json)?, mnemonic: None, policy: None })
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
        Self::derive_mnemonic(phrase, SOLANA_DERIVATION_PATH)
    }

    /// SLIP-10 ed25519 derivation of a mnemonic at an explicit index path.
    /// Every level is hardened (ed25519 supports hardened derivation only).
    fn derive_mnemonic(phrase: &str, path: &[u32]) -> Result<Self> {
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

        // Derive hardened child keys along the path
        for &index in path {
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

    /// Import from a BIP39 mnemonic at a caller-supplied derivation path
    /// (e.g. `m/44'/501'/1'/0'` for a non-default account). Use
    /// [`account_derivation_path`] to build a path from an account index.
    pub fn from_mnemonic_at(phrase: &str, derivation_path: &str) -> Result<Self> {
        let path = parse_derivation_path(derivation_path)?;
        Self::derive_mnemonic(phrase, &path)
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
        write_key_file(path, json.as_bytes())
    }

    /// Save to the default path ~/.config/rwa/key.json.
    pub fn save_default(&self) -> Result<PathBuf> {
        let path = default_key_path()?;
        self.save(&path)?;
        Ok(path)
    }

    /// The 64-byte secret+public keypair (solana-keygen layout), for export.
    #[must_use]
    pub fn to_keypair_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut kp = Zeroizing::new(Vec::with_capacity(64));
        kp.extend_from_slice(self.signing_key.as_bytes());
        kp.extend_from_slice(self.verifying_key().as_bytes());
        kp
    }

    /// Base58 of the 64-byte keypair — the format Phantom/Solflare import as
    /// a "private key".
    #[must_use]
    pub fn to_base58_keypair(&self) -> Zeroizing<String> {
        Zeroizing::new(bs58::encode(self.to_keypair_bytes().as_slice()).into_string())
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

    /// Sign a Jupiter swap transaction after verifying its instructions
    /// match the caller's intent.
    ///
    /// Refuses to sign — without ever touching the signing key — if the tx
    /// debits the wrong mint, wrong amount, or routes to an ATA we don't own.
    /// This is the last line of defence against a compromised quote endpoint.
    pub fn sign_jupiter_swap(&self, tx_base64: &str, expected: &ExpectedSwap) -> Result<String> {
        verify::decode_and_verify(tx_base64, expected)?;
        self.sign_transaction(tx_base64)
    }

    /// Sign a serialized Solana transaction (legacy or versioned).
    ///
    /// Decodes base64, finds the correct signature slot by matching our pubkey
    /// in the transaction's account keys, signs the message, and returns
    /// re-encoded base64.
    ///
    /// Crate-private on purpose: external swap signing must go through
    /// `sign_jupiter_swap`, which verifies the transaction against the
    /// caller's intent before touching the key.
    pub(crate) fn sign_transaction(&self, tx_base64: &str) -> Result<String> {
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

        // Find our pubkey among the required signers.
        //
        // Audit L2: `num_required_sigs` comes straight from the untrusted
        // message header and must never be trusted past the *outer*
        // signature-slot count (`num_sigs`, decoded from the transaction's
        // own compact-u16 prefix). A crafted quote could claim a large
        // `num_required_sigs` while shipping far fewer real signature slots
        // — clamp the search to whichever of the three counts is smallest so
        // `sig_index` can never point past the real signature area.
        let verifying_key = self.signing_key.verifying_key();
        let our_pubkey = verifying_key.as_bytes();
        let search_limit = std::cmp::min(std::cmp::min(num_keys as usize, num_required_sigs), num_sigs as usize);
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

        // Write signature into the correct slot. `sig_index < search_limit <=
        // num_sigs` already guarantees this is in-bounds (sigs_end + 4 <=
        // tx_bytes.len() was checked above), but keep an explicit bounds
        // check as defense-in-depth against any future change to the
        // invariant above: an out-of-bounds write must error, never panic.
        let slot_offset = sigs_start + sig_index * 64;
        if slot_offset + 64 > tx_bytes.len() {
            return Err(eyre!(
                "Signature slot offset out of bounds — malformed transaction header"
            ));
        }
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

/// Detect whether a key file is age-encrypted by inspecting its header, rather
/// than trusting the filename. age v1 files (binary or armored) begin with a
/// recognizable marker; a plaintext solana-keygen key is a JSON array (`[`).
/// Used by the named-wallet loader, where paths are arbitrary.
pub fn is_age_encrypted(path: &Path) -> Result<bool> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| eyre!("Cannot open key file {}: {e}", path.display()))?;
    let mut head = [0u8; 64];
    let n = f
        .read(&mut head)
        .map_err(|e| eyre!("Cannot read key file {}: {e}", path.display()))?;
    let head = &head[..n];
    Ok(head.starts_with(b"age-encryption.org/v1")
        || head.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----"))
}

// Canonical compact-u16 decoder lives in `parser`; reused here for signing.
use parser::decode_compact_u16;

/// Write key material so the file is never readable by group/other: on unix
/// the file is created with mode 0o600 before any bytes land (no umask
/// window). Pre-existing files are repaired to 0o600 as well, since `mode()`
/// only applies at creation time.
fn write_key_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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

    #[cfg(unix)]
    #[test]
    fn save_creates_file_with_0600_even_over_existing_looser_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rwa-wallet-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.json");

        let w = Wallet::generate();
        w.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // A pre-existing world-readable file must be repaired, not inherited.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        w.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Audit L3: compile-level pin that `DecryptedWallet.mnemonic` (and, by
    /// construction, the `Payload.mnemonic` it's deserialized from) is
    /// exactly `Option<Zeroizing<String>>` — `assert_zeroizing` only
    /// type-checks against that field type, so a regression back to a bare
    /// `Option<String>` fails to compile, not just fails at runtime. Paired
    /// with a behavioral round trip: the phrase set at save time is still
    /// the phrase read back after load, proving the type change is
    /// otherwise a no-op.
    #[test]
    fn decrypted_wallet_mnemonic_is_zeroizing() {
        fn assert_zeroizing(_: &Option<Zeroizing<String>>) {}

        let dir = std::env::temp_dir().join(format!("rwa-zeroizing-pin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.age");
        let (w, phrase) = Wallet::generate_with_mnemonic(12).unwrap();
        w.save_encrypted_with_mnemonic(&path, "pass123", Some(phrase.as_str())).unwrap();

        let dw = Wallet::load_encrypted_payload(&path, "pass123").unwrap();
        assert_zeroizing(&dw.mnemonic);
        assert_eq!(dw.mnemonic.as_deref().map(|s| s.as_str()), Some(phrase.as_str()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encrypted_mnemonic_roundtrip_and_legacy_compat() {
        let dir = std::env::temp_dir().join(format!("rwa-mn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Object payload: mnemonic survives the roundtrip and restores the
        // same wallet the phrase derives.
        let (w, phrase) = Wallet::generate_with_mnemonic(12).unwrap();
        assert_eq!(phrase.split_whitespace().count(), 12);
        let p = dir.join("k.age");
        w.save_encrypted_with_mnemonic(&p, "TestPass2026!secure", Some(phrase.as_str())).unwrap();
        let (loaded, stored) = Wallet::from_encrypted_file_full(&p, "TestPass2026!secure").unwrap();
        assert_eq!(loaded.pubkey(), w.pubkey());
        assert_eq!(stored.as_deref(), Some(phrase.as_str()));
        assert_eq!(Wallet::from_mnemonic(&phrase).unwrap().pubkey(), w.pubkey());

        // Legacy bare-array payload still loads, with no mnemonic.
        let p2 = dir.join("legacy.age");
        w.save_encrypted(&p2, "TestPass2026!secure").unwrap();
        let (loaded2, stored2) = Wallet::from_encrypted_file_full(&p2, "TestPass2026!secure").unwrap();
        assert_eq!(loaded2.pubkey(), w.pubkey());
        assert!(stored2.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_with_mnemonic_24_words() {
        let (w, phrase) = Wallet::generate_with_mnemonic(24).unwrap();
        assert_eq!(phrase.split_whitespace().count(), 24);
        assert_eq!(Wallet::from_mnemonic(&phrase).unwrap().pubkey(), w.pubkey());
    }

    /// Entropy invariant — the regression guard against the "weak RNG" wallet-
    /// drain class (Trust Wallet 2023 / CVE-2023-31290, Milk Sad, Coinspect's
    /// "Ill Bloom"). Those wallets shrank the effective keyspace to ~2^32 by
    /// seeding a non-CSPRNG (MT19937) from 32-bit wall-clock time, so seeds
    /// were brute-forceable. Two properties defeat that class and are pinned
    /// here:
    ///   1. Width — a 12-word phrase must recover to a full 16 bytes (128 bits)
    ///      of entropy and a 24-word phrase to 32 bytes (256 bits). Truncating
    ///      `fill_bytes` or mis-sizing the buffer would fail this.
    ///   2. No collisions — many back-to-back generations must all differ. A
    ///      time-seeded generator emits identical entropy for calls landing in
    ///      the same clock tick; a real CSPRNG (rand 0.8 ThreadRng = ChaCha12
    ///      seeded from the OS via getrandom) never collides here. The birthday
    ///      odds of a genuine 128-bit collision across 256 draws are ~2^-113 —
    ///      a failure means the source degraded, not bad luck.
    #[test]
    fn generated_mnemonic_carries_full_csprng_entropy() {
        use std::collections::HashSet;

        // Width: recovered entropy is exactly 16 / 32 bytes, and never the
        // all-zero buffer a no-op fill would leave behind.
        let (_w12, p12) = Wallet::generate_with_mnemonic(12).unwrap();
        let e12 = p12.parse::<bip39::Mnemonic>().unwrap().to_entropy();
        assert_eq!(e12.len(), 16, "12-word phrase must encode 128 bits of entropy");
        assert!(e12.iter().any(|&b| b != 0), "entropy must not be all-zero");

        let (_w24, p24) = Wallet::generate_with_mnemonic(24).unwrap();
        let e24 = p24.parse::<bip39::Mnemonic>().unwrap().to_entropy();
        assert_eq!(e24.len(), 32, "24-word phrase must encode 256 bits of entropy");

        // No collisions across 256 rapid draws — the direct test for a
        // time-seeded / constant PRNG, which would repeat within a clock tick.
        let mut seen = HashSet::new();
        for _ in 0..256 {
            let (_w, phrase) = Wallet::generate_with_mnemonic(12).unwrap();
            let entropy = phrase.parse::<bip39::Mnemonic>().unwrap().to_entropy();
            assert!(
                seen.insert(entropy),
                "two generations produced identical entropy — source is not a CSPRNG"
            );
        }
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
    fn from_mnemonic_derives_the_reference_solana_address() {
        // Reference vector for the standard BIP-39 test mnemonic at
        // m/44'/501'/0'/0'. Independently derived: BIP-39 seed via
        // PBKDF2-HMAC-SHA512(mnemonic, "mnemonic", 2048) then SLIP-10 ed25519
        // chain (python stdlib hmac/hashlib, no shared code with this crate);
        // secret 37df573b3ac4ad5b522e064e25b63ea16bcbe79d449e81a0268d1047948bb445.
        // This is also the address Phantom/Solflare derive for account #0 —
        // a wrong path, endianness, or hardened-index bug would produce a
        // stable, valid-looking, but DIFFERENT wallet and silently strand an
        // imported seed.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let w = Wallet::from_mnemonic(phrase).unwrap();
        assert_eq!(w.pubkey(), "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk");

        // The same secret imported directly must land on the same keypair —
        // pins from_private_key's hex path against the same vector.
        let direct = Wallet::from_private_key(
            "37df573b3ac4ad5b522e064e25b63ea16bcbe79d449e81a0268d1047948bb445",
        )
        .unwrap();
        assert_eq!(direct.pubkey(), w.pubkey());
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

    const ABANDON: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn parse_derivation_path_standard() {
        assert_eq!(parse_derivation_path("m/44'/501'/1'/0'").unwrap(), vec![44, 501, 1, 0]);
    }

    #[test]
    fn parse_derivation_path_accepts_h_marker_and_trailing_slash() {
        assert_eq!(parse_derivation_path("m/44h/501h/2h/0h/").unwrap(), vec![44, 501, 2, 0]);
    }

    #[test]
    fn parse_derivation_path_rejects_bad_start_and_nonnumeric() {
        assert!(parse_derivation_path("44'/501'/0'/0'").is_err());
        assert!(parse_derivation_path("m/44'/xyz'/0'/0'").is_err());
        assert!(parse_derivation_path("m").is_err());
    }

    #[test]
    fn parse_derivation_path_rejects_empty_and_overflow_index() {
        // No indices after 'm'.
        assert!(parse_derivation_path("m/").is_err());
        // Index with the hardening bit already set is out of range (we harden).
        assert!(parse_derivation_path("m/44'/501'/2147483648'/0'").is_err());
        // Largest valid unhardened index is fine.
        assert_eq!(
            parse_derivation_path("m/2147483647'").unwrap(),
            vec![2_147_483_647]
        );
    }

    #[test]
    fn account_derivation_path_builds_standard_solana_path() {
        assert_eq!(account_derivation_path(0), "m/44'/501'/0'/0'");
        assert_eq!(account_derivation_path(2), "m/44'/501'/2'/0'");
    }

    #[test]
    fn from_mnemonic_at_default_path_matches_from_mnemonic() {
        assert_eq!(
            Wallet::from_mnemonic_at(ABANDON, "m/44'/501'/0'/0'").unwrap().pubkey(),
            Wallet::from_mnemonic(ABANDON).unwrap().pubkey()
        );
    }

    #[test]
    fn from_mnemonic_at_account_one_matches_independent_reference() {
        // Independently derived (python SLIP-10 ed25519, no shared code) at
        // m/44'/501'/1'/0' — this is Phantom/Solflare "Account 2".
        assert_eq!(
            Wallet::from_mnemonic_at(ABANDON, "m/44'/501'/1'/0'").unwrap().pubkey(),
            "Hh8QwFUA6MtVu1qAoq12ucvFHNwCcVTV7hpWjeY1Hztb"
        );
        assert_ne!(
            Wallet::from_mnemonic_at(ABANDON, "m/44'/501'/1'/0'").unwrap().pubkey(),
            Wallet::from_mnemonic(ABANDON).unwrap().pubkey()
        );
    }

    #[test]
    fn from_mnemonic_at_rejects_bad_path() {
        assert!(Wallet::from_mnemonic_at(ABANDON, "not-a-path").is_err());
    }

    #[test]
    fn sign_transaction_rejects_empty() {
        let w = Wallet::generate();
        // Empty base64 should fail
        assert!(w.sign_transaction("").is_err());
    }

    /// Audit L2: a hand-rolled parser must not trust the message-header
    /// `num_required_sigs` beyond the bounds of the outer signature-slot
    /// count. Craft a v0 message that claims 10 required signers (with our
    /// wallet's pubkey placed as key index 9, the LAST claimed signer) while
    /// the real signature area (the outer compact-u16 prefix) has only 1
    /// slot. Before the fix, `search_limit = min(num_keys, num_required_sigs)`
    /// happily walks up to index 9, finds our pubkey, and computes
    /// `slot_offset = sigs_start + 9*64 = 577` against a 390-byte buffer —
    /// `tx_bytes[577..641].copy_from_slice(..)` panics (index out of range).
    /// After the fix this must be a clean `Err`, never a panic.
    #[test]
    fn sign_transaction_rejects_header_sig_count_mismatch() {
        use base64::Engine;

        let w = Wallet::generate();
        let owner_bytes: [u8; 32] =
            bs58::decode(w.pubkey()).into_vec().unwrap().try_into().unwrap();

        // 10 keys; our pubkey is the last one (index 9).
        let mut keys = vec![[0u8; 32]; 10];
        keys[9] = owner_bytes;

        // v0 tag + header claiming num_required_sigs = 10 (a lie: only 1
        // signature slot actually exists below).
        let mut message = vec![0x80u8, 10, 0, 0];
        encode_compact_u16_test(keys.len() as u16, &mut message);
        for k in &keys {
            message.extend_from_slice(k);
        }

        let mut tx = Vec::new();
        encode_compact_u16_test(1, &mut tx); // outer sig-slot count = 1 (the truth)
        tx.extend_from_slice(&[0u8; 64]); // the single real signature slot
        tx.extend_from_slice(&message);

        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

        assert!(
            w.sign_transaction(&tx_b64).is_err(),
            "header claiming more required sigs than the outer sig-slot count must be rejected, not panic"
        );
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

    // ── sign_jupiter_swap (verifies-then-signs wrapper) ──────────────────

    /// Builds a v0 Solana transaction that debits `amount` from
    /// ATA(wallet, input_mint) into a writable ATA(wallet, output_mint).
    /// Returns the base64 transaction. Mirrors the test fixture in
    /// `verify::tests` but parameterised on the real wallet pubkey.
    fn build_jupiter_like_tx(
        owner: &[u8; 32],
        input_mint: &[u8; 32],
        output_mint: &[u8; 32],
        amount: u64,
    ) -> String {
        use crate::spl::{TOKEN_PROGRAM_ID, derive_ata_pubkey};
        use base64::Engine;

        let input_ata = derive_ata_pubkey(owner, input_mint, &TOKEN_PROGRAM_ID).unwrap();
        let output_ata = derive_ata_pubkey(owner, output_mint, &TOKEN_PROGRAM_ID).unwrap();
        let intermediate = [0x44u8; 32];
        let blockhash = [0x55u8; 32];

        let keys: Vec<[u8; 32]> = vec![
            *owner,
            input_ata,
            intermediate,
            output_ata,
            TOKEN_PROGRAM_ID,
            *input_mint,
        ];

        let mut msg = vec![0x80u8, 1, 0, 2];
        encode_compact_u16_test(keys.len() as u16, &mut msg);
        for k in &keys {
            msg.extend_from_slice(k);
        }
        msg.extend_from_slice(&blockhash);
        encode_compact_u16_test(1, &mut msg);
        msg.push(4); // program_id_index = TOKEN_PROGRAM_ID
        encode_compact_u16_test(4, &mut msg); // 4 account indices
        msg.extend_from_slice(&[1u8, 5, 2, 0]); // source, mint, dest, authority

        let mut ix_data = vec![12u8]; // TransferChecked
        ix_data.extend_from_slice(&amount.to_le_bytes());
        ix_data.push(6); // decimals
        encode_compact_u16_test(ix_data.len() as u16, &mut msg);
        msg.extend_from_slice(&ix_data);
        // Zero ALTs.
        encode_compact_u16_test(0, &mut msg);

        let mut tx = Vec::new();
        encode_compact_u16_test(1, &mut tx); // 1 sig slot
        tx.extend_from_slice(&[0u8; 64]);
        tx.extend_from_slice(&msg);
        base64::engine::general_purpose::STANDARD.encode(&tx)
    }

    fn encode_compact_u16_test(val: u16, out: &mut Vec<u8>) {
        if val < 0x80 {
            out.push(val as u8);
        } else if val < 0x4000 {
            out.push((val & 0x7f) as u8 | 0x80);
            out.push((val >> 7) as u8);
        } else {
            out.push((val & 0x7f) as u8 | 0x80);
            out.push(((val >> 7) & 0x7f) as u8 | 0x80);
            out.push((val >> 14) as u8);
        }
    }

    #[test]
    fn sign_jupiter_swap_signs_when_intent_matches() {
        let w = Wallet::generate();
        let owner_bytes: [u8; 32] = bs58::decode(w.pubkey()).into_vec().unwrap().try_into().unwrap();
        let input_mint = [0x22u8; 32];
        let output_mint = [0x33u8; 32];
        let amount = 1_000_000u64;
        let tx = build_jupiter_like_tx(&owner_bytes, &input_mint, &output_mint, amount);

        let expected = ExpectedSwap {
            input_mint,
            input_amount: amount,
            output_mint,
            owner_pubkey: owner_bytes,
        };
        let signed = w.sign_jupiter_swap(&tx, &expected)
            .expect("legitimate Jupiter-shaped tx should be signed");
        // A real ed25519 signature over the message bytes must land in our
        // signature slot — "differs from input" would also pass for garbage.
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&signed)
            .expect("signed tx is base64");
        let (num_sigs, prefix) = decode_compact_u16(&bytes).unwrap();
        let sigs_end = prefix + num_sigs as usize * 64;
        let message = &bytes[sigs_end..];
        let pubkey_bytes: [u8; 32] =
            bs58::decode(w.pubkey()).into_vec().unwrap().try_into().unwrap();
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&pubkey_bytes).unwrap();
        let our_sig = (0..num_sigs as usize)
            .map(|i| &bytes[prefix + i * 64..prefix + (i + 1) * 64])
            .find_map(|slot| {
                let sig = ed25519_dalek::Signature::from_bytes(slot.try_into().unwrap());
                vk.verify_strict(message, &sig).ok()
            });
        assert!(
            our_sig.is_some(),
            "one signature slot must cryptographically verify against our pubkey over the message"
        );
    }

    #[test]
    fn sign_jupiter_swap_refuses_amount_mismatch() {
        let w = Wallet::generate();
        let owner_bytes: [u8; 32] = bs58::decode(w.pubkey()).into_vec().unwrap().try_into().unwrap();
        let input_mint = [0x22u8; 32];
        let output_mint = [0x33u8; 32];
        let amount = 1_000_000u64;
        let tx = build_jupiter_like_tx(&owner_bytes, &input_mint, &output_mint, amount);

        // Caller expects half the amount → wallet must refuse to sign.
        let expected = ExpectedSwap {
            input_mint,
            input_amount: amount / 2,
            output_mint,
            owner_pubkey: owner_bytes,
        };
        let result = w.sign_jupiter_swap(&tx, &expected);
        assert!(result.is_err(), "amount mismatch must abort signing");
        let err = result.unwrap_err();
        let kind = err.downcast_ref::<VerifyError>().expect("structured VerifyError");
        assert!(matches!(kind, VerifyError::InputAmountMismatch { .. }));
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

    #[test]
    fn detects_encryption_by_content() {
        let plain = TempFile::new("key.json");
        let enc = TempFile::new("key.age");
        let w = Wallet::generate();
        w.save(plain.path()).unwrap();
        w.save_encrypted(enc.path(), "TestPass2026!secure").unwrap();

        assert!(!is_age_encrypted(plain.path()).unwrap(), "plaintext JSON => false");
        assert!(is_age_encrypted(enc.path()).unwrap(), "age file => true");
    }

    // ── payload v3 (SendPolicy) ───────────────────────────────────────────
    // `tempfile` isn't a dev-dep of rwa-ondo, so these mirror the dir-based
    // temp-file style already used by `encrypted_mnemonic_roundtrip_and_legacy_compat`.

    #[test]
    fn payload_v3_roundtrip_preserves_key_mnemonic_policy() {
        let dir = std::env::temp_dir().join(format!("rwa-payload-v3-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.age");
        let (w, phrase) = Wallet::generate_with_mnemonic(12).unwrap();
        let mut policy = SendPolicy::default();
        policy.allow("Dn9Qxyz111111111111111111111111111111111111", Some("cold"), "2026-07-16T00:00:00Z").unwrap();
        w.save_encrypted_payload(&path, "pass123", Some(phrase.as_str()), Some(&policy)).unwrap();

        let loaded = Wallet::load_encrypted_payload(&path, "pass123").unwrap();
        assert_eq!(loaded.wallet.pubkey(), w.pubkey());
        assert_eq!(loaded.mnemonic.as_deref().map(|s| s.as_str()), Some(phrase.as_str()));
        let p = loaded.policy.unwrap();
        assert!(p.permits("Dn9Qxyz111111111111111111111111111111111111"));
        assert!(!p.permits("SomeOtherAddress11111111111111111111111111"));
        assert_eq!(p.allowed_recipients[0].label.as_deref(), Some("cold"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn payload_v2_and_v1_load_with_policy_none() {
        let dir = std::env::temp_dir().join(format!("rwa-payload-v2v1-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // v2: written via the legacy wrapper (object without "policy").
        let v2 = dir.join("v2.age");
        let (w, phrase) = Wallet::generate_with_mnemonic(12).unwrap();
        w.save_encrypted_with_mnemonic(&v2, "pass123", Some(phrase.as_str())).unwrap();
        let loaded = Wallet::load_encrypted_payload(&v2, "pass123").unwrap();
        assert_eq!(loaded.mnemonic.as_deref().map(|s| s.as_str()), Some(phrase.as_str()));
        assert!(loaded.policy.is_none());
        // v1: bare array (written via save_encrypted).
        let v1 = dir.join("v1.age");
        w.save_encrypted(&v1, "pass123").unwrap();
        let loaded = Wallet::load_encrypted_payload(&v1, "pass123").unwrap();
        assert!(loaded.mnemonic.is_none());
        assert!(loaded.policy.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn policy_allow_rejects_duplicates_and_remove_reports_presence() {
        let mut p = SendPolicy::default();
        p.allow("Addr1", None, "2026-07-16T00:00:00Z").unwrap();
        assert!(p.allow("Addr1", Some("again"), "2026-07-16T00:00:00Z").is_err());
        assert!(p.remove("Addr1"));
        assert!(!p.remove("Addr1"));
    }
}
