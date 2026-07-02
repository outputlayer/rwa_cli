//! SPL token primitives shared by the `solana` and `wallet` layers: program
//! ids and Associated-Token-Account derivation. A leaf module with no crate
//! dependencies, so `wallet` (transaction verification) and `solana` (RPC,
//! transfers) can both use it without depending on each other.

use eyre::{eyre, Result};

/// SPL Token Program ID.
pub const TOKEN_PROGRAM_ID: [u8; 32] = [
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172,
    28, 180, 133, 237, 95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
];
/// SPL Token-2022 Program ID (GM tokens).
pub const TOKEN_2022_PROGRAM_ID: [u8; 32] = [
    6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218,
    182, 26, 252, 77, 131, 185, 13, 39, 254, 189, 249, 40, 216, 161, 139, 252,
];
/// Associated Token Account Program ID.
pub(crate) const ATA_PROGRAM: [u8; 32] = [
    140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131,
    11, 90, 19, 153, 218, 255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89,
];

/// Wrapped-SOL mint — native SOL in SPL form.
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// True when `mint` is wrapped SOL. Routes that output SOL unwrap it, so the
/// credit lands as native lamports on the owner account rather than a token
/// ATA — verification must account for that.
pub(crate) fn is_wsol(mint: &[u8; 32]) -> bool {
    static WSOL: std::sync::LazyLock<[u8; 32]> = std::sync::LazyLock::new(|| {
        bs58::decode(WSOL_MINT)
            .into_vec()
            .expect("const mint decodes")
            .try_into()
            .expect("32 bytes")
    });
    mint == &*WSOL
}

/// Derive the ATA pubkey for fixed-size keys.
///
/// `owner` and `mint` are 32-byte Solana pubkeys; `token_program` is either
/// the SPL Token or Token-2022 program id. Returns the 32-byte ATA pubkey.
pub fn derive_ata_pubkey(
    owner: &[u8; 32],
    mint: &[u8; 32],
    token_program: &[u8; 32],
) -> Result<[u8; 32]> {
    let v = derive_ata(owner.as_slice(), mint.as_slice(), token_program)?;
    let arr: [u8; 32] = v
        .as_slice()
        .try_into()
        .map_err(|_| eyre!("derived ATA wrong length"))?;
    Ok(arr)
}

/// The owner's ATAs for `mint` under both token programs, in
/// `[Token, Token-2022]` order. The wrong-program candidate simply doesn't
/// exist on-chain; the real ATA is whichever the mint actually uses.
pub(crate) fn mint_atas(owner: &[u8; 32], mint: &[u8; 32]) -> Result<[[u8; 32]; 2]> {
    Ok([
        derive_ata_pubkey(owner, mint, &TOKEN_PROGRAM_ID)?,
        derive_ata_pubkey(owner, mint, &TOKEN_2022_PROGRAM_ID)?,
    ])
}

/// Derive Associated Token Account address.
pub(crate) fn derive_ata(owner: &[u8], mint: &[u8], token_program: &[u8; 32]) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};

    // PDA seeds: [owner, token_program, mint]
    // Program: ATA program
    let seeds: &[&[u8]] = &[owner, token_program.as_slice(), mint];

    // Find PDA: try nonce from 255 down to 0
    for nonce in (0..=255u8).rev() {
        let mut hasher = Sha256::new();
        for seed in seeds {
            hasher.update(seed);
        }
        hasher.update([nonce]);
        hasher.update(ATA_PROGRAM);
        hasher.update(b"ProgramDerivedAddress");
        let hash = hasher.finalize();

        // Valid PDA must NOT be on the Ed25519 curve
        if !is_on_curve(&hash) {
            return Ok(hash.to_vec());
        }
    }

    Err(eyre!("Failed to derive ATA — no valid PDA found"))
}

/// Check if a 32-byte key is on the Ed25519 curve.
fn is_on_curve(bytes: &[u8]) -> bool {
    let Ok(arr) = <[u8; 32]>::try_from(bytes) else {
        return false;
    };
    ed25519_dalek::VerifyingKey::from_bytes(&arr).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_ata_produces_32_bytes_and_is_deterministic() {
        let owner = bs58::decode("11111111111111111111111111111111").into_vec().unwrap();
        let mint = bs58::decode("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").into_vec().unwrap();
        let ata1 = derive_ata(&owner, &mint, &TOKEN_PROGRAM_ID).unwrap();
        let ata2 = derive_ata(&owner, &mint, &TOKEN_PROGRAM_ID).unwrap();
        assert_eq!(ata1.len(), 32);
        assert_eq!(ata1, ata2);
    }

    #[test]
    fn program_ids_match_their_base58_forms() {
        let tok = bs58::decode("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").into_vec().unwrap();
        assert_eq!(TOKEN_PROGRAM_ID.as_slice(), tok.as_slice());
        let t2022 = bs58::decode("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb").into_vec().unwrap();
        assert_eq!(TOKEN_2022_PROGRAM_ID.as_slice(), t2022.as_slice());
        let ata = bs58::decode("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").into_vec().unwrap();
        assert_eq!(ATA_PROGRAM.as_slice(), ata.as_slice());
    }

    #[test]
    fn mint_atas_differ_per_token_program() {
        let owner: [u8; 32] = bs58::decode("11111111111111111111111111111111")
            .into_vec().unwrap().try_into().unwrap();
        let mint: [u8; 32] = bs58::decode("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
            .into_vec().unwrap().try_into().unwrap();
        let atas = mint_atas(&owner, &mint).unwrap();
        assert_ne!(atas[0], atas[1]);
    }
}
