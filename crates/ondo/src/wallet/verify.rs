//! Sign-time verification for Jupiter swap transactions.
//!
//! Decodes the base64 transaction returned by Jupiter and confirms that the
//! instructions actually move `expected.input_amount` of `expected.input_mint`
//! out of the user's ATA into a swap whose final destination is the user's
//! ATA for `expected.output_mint`. This is the last line of defence against a
//! compromised or malicious quote endpoint substituting recipient/amount.
//!
//! The parser is intentionally tolerant: unknown instructions (compute-budget
//! tweaks, ALT extends, route-specific helpers) are allowed. We only fail when
//! a *required* swap leg is missing or contradicts the expected swap.

use base64::Engine;
use eyre::{Result, eyre};

use crate::solana::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, derive_ata_pubkey};

/// What the caller intended to swap. Built once from user input
/// (e.g. `prepare_buy` / `prepare_sell`) and threaded through to the signer.
#[derive(Debug, Clone)]
pub struct ExpectedSwap {
    pub input_mint: [u8; 32],
    pub input_amount: u64,
    pub output_mint: [u8; 32],
    pub owner_pubkey: [u8; 32],
}

impl ExpectedSwap {
    /// Build from base58 strings — convenience for callers in `usecases::*`.
    pub fn from_base58(
        input_mint: &str,
        input_amount: u64,
        output_mint: &str,
        owner_pubkey: &str,
    ) -> Result<Self> {
        Ok(Self {
            input_mint: decode_b58_32(input_mint, "input_mint")?,
            input_amount,
            output_mint: decode_b58_32(output_mint, "output_mint")?,
            owner_pubkey: decode_b58_32(owner_pubkey, "owner_pubkey")?,
        })
    }
}

/// Structured failure mode of `decode_and_verify`. Each variant maps to one
/// of the AC-1.x acceptance scenarios.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// Transaction bytes could not be decoded / parsed at all.
    MalformedTransaction(String),
    /// No instruction debits the expected input mint from the owner.
    MissingInputTransfer,
    /// An input transfer was found, but for the wrong amount.
    InputAmountMismatch { expected: u64, found: u64 },
    /// The transferred amount left the wrong mint (e.g. user expected USDC out, tx moves SOL).
    InputMintMismatch,
    /// The wallet signer is not the authority on the input transfer.
    OwnerMismatch,
    /// No output ATA owned by `expected.owner_pubkey` for `expected.output_mint`
    /// is present as a writable account in the transaction.
    OutputAtaMissing,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedTransaction(s) => write!(f, "malformed transaction: {s}"),
            Self::MissingInputTransfer => f.write_str(
                "Jupiter transaction does not debit the expected input mint from this wallet",
            ),
            Self::InputAmountMismatch { expected, found } => write!(
                f,
                "amount mismatch: expected {expected} raw units, transaction transfers {found}",
            ),
            Self::InputMintMismatch => f.write_str(
                "input mint mismatch: transaction debits a different mint than expected",
            ),
            Self::OwnerMismatch => f.write_str(
                "owner mismatch: signing authority on input transfer is not this wallet",
            ),
            Self::OutputAtaMissing => f.write_str(
                "output ATA mismatch: transaction does not credit this wallet's ATA for the expected output mint",
            ),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Decode and verify a base64 Solana transaction against `expected`.
///
/// Returns `Ok(())` if the transaction credibly performs the intended swap.
/// On any contradiction returns a structured `VerifyError`.
pub fn decode_and_verify(tx_base64: &str, expected: &ExpectedSwap) -> Result<()> {
    let engine = base64::engine::general_purpose::STANDARD;
    let tx_bytes = engine
        .decode(tx_base64)
        .map_err(|e| VerifyError::MalformedTransaction(format!("base64: {e}")))?;
    let parsed = parse_message(&tx_bytes)
        .map_err(|e| VerifyError::MalformedTransaction(e.to_string()))?;
    verify_parsed(&parsed, expected)
}

// ── Internal types ────────────────────────────────────────────────────────

/// What we managed to extract from the v0/legacy message.
#[derive(Debug)]
struct ParsedMessage {
    /// Static account keys, in message order. (Address-table-lookup keys are
    /// appended after these but we don't know their values without the table —
    /// for verification we only rely on static keys; ALT-only writes never
    /// carry the user's ATA.)
    keys: Vec<[u8; 32]>,
    /// `keys` index ranges that are signer / writable, derived from the header.
    num_required_sigs: u8,
    num_readonly_signed: u8,
    num_readonly_unsigned: u8,
    /// True if ALT lookups are present — extra writable/readonly account slots.
    /// We don't resolve them, but we do treat `OutputAtaMissing` more leniently
    /// when ALTs are in play.
    has_alt: bool,
    /// Instructions in declared order.
    ixs: Vec<ParsedInstruction>,
}

#[derive(Debug)]
struct ParsedInstruction {
    program_id_index: u8,
    account_indices: Vec<u8>,
    data: Vec<u8>,
}

impl ParsedMessage {
    fn program_id(&self, ix: &ParsedInstruction) -> Option<&[u8; 32]> {
        self.keys.get(ix.program_id_index as usize)
    }
    fn account_key(&self, idx: u8) -> Option<&[u8; 32]> {
        self.keys.get(idx as usize)
    }
    fn writable_static_indices(&self) -> impl Iterator<Item = usize> + '_ {
        // Writable signers: 0..num_required_sigs - num_readonly_signed
        // Writable unsigned: num_required_sigs..(keys.len() - num_readonly_unsigned)
        let n_keys = self.keys.len();
        let writable_signed_end =
            (self.num_required_sigs as usize).saturating_sub(self.num_readonly_signed as usize);
        let writable_unsigned_start = self.num_required_sigs as usize;
        let writable_unsigned_end =
            n_keys.saturating_sub(self.num_readonly_unsigned as usize);
        (0..writable_signed_end)
            .chain(writable_unsigned_start..writable_unsigned_end)
    }
}

// ── Parser (v0 + legacy) ──────────────────────────────────────────────────

fn parse_message(tx_bytes: &[u8]) -> Result<ParsedMessage> {
    // Skip signatures: compact-u16 count + 64*count bytes
    let (num_sigs, sig_count_len) = decode_compact_u16(tx_bytes)?;
    let sigs_end = sig_count_len + (num_sigs as usize) * 64;
    if tx_bytes.len() < sigs_end + 4 {
        return Err(eyre!("transaction too short"));
    }
    let msg = &tx_bytes[sigs_end..];

    // V0 message starts with 0x80 (tag), legacy starts with the header byte (< 0x80).
    let mut cur = 0usize;
    let is_v0 = msg[0] & 0x80 != 0;
    if is_v0 {
        cur += 1;
    }

    // Header: 3 bytes
    if msg.len() < cur + 3 {
        return Err(eyre!("truncated header"));
    }
    let num_required_sigs = msg[cur];
    let num_readonly_signed = msg[cur + 1];
    let num_readonly_unsigned = msg[cur + 2];
    cur += 3;

    // Static account keys
    let (num_keys, n) = decode_compact_u16(&msg[cur..])?;
    cur += n;
    let mut keys = Vec::with_capacity(num_keys as usize);
    for _ in 0..num_keys {
        if msg.len() < cur + 32 {
            return Err(eyre!("truncated account keys"));
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&msg[cur..cur + 32]);
        keys.push(k);
        cur += 32;
    }

    // Recent blockhash (32 bytes)
    if msg.len() < cur + 32 {
        return Err(eyre!("truncated blockhash"));
    }
    cur += 32;

    // Instructions
    let (num_ix, n) = decode_compact_u16(&msg[cur..])?;
    cur += n;
    let mut ixs = Vec::with_capacity(num_ix as usize);
    for _ in 0..num_ix {
        if msg.len() <= cur {
            return Err(eyre!("truncated instructions"));
        }
        let program_id_index = msg[cur];
        cur += 1;
        let (n_acc, n_consumed) = decode_compact_u16(&msg[cur..])?;
        cur += n_consumed;
        if msg.len() < cur + n_acc as usize {
            return Err(eyre!("truncated instruction account indices"));
        }
        let account_indices = msg[cur..cur + n_acc as usize].to_vec();
        cur += n_acc as usize;
        let (data_len, n_consumed) = decode_compact_u16(&msg[cur..])?;
        cur += n_consumed;
        if msg.len() < cur + data_len as usize {
            return Err(eyre!("truncated instruction data"));
        }
        let data = msg[cur..cur + data_len as usize].to_vec();
        cur += data_len as usize;
        ixs.push(ParsedInstruction {
            program_id_index,
            account_indices,
            data,
        });
    }

    // Address table lookups (V0 only). We ignore their resolved keys but record
    // their presence so callers know not to enforce strict output-ATA checks
    // against static keys alone.
    let mut has_alt = false;
    if is_v0 && cur < msg.len() {
        let (n_alt, n_consumed) = decode_compact_u16(&msg[cur..])?;
        cur += n_consumed;
        for _ in 0..n_alt {
            // 32 bytes table account + writable indices vec + readonly indices vec
            if msg.len() < cur + 32 {
                return Err(eyre!("truncated ALT entry"));
            }
            cur += 32;
            let (n_w, n_consumed) = decode_compact_u16(&msg[cur..])?;
            cur += n_consumed;
            cur += n_w as usize;
            if msg.len() < cur {
                return Err(eyre!("truncated ALT writable indices"));
            }
            let (n_r, n_consumed) = decode_compact_u16(&msg[cur..])?;
            cur += n_consumed;
            cur += n_r as usize;
            if msg.len() < cur {
                return Err(eyre!("truncated ALT readonly indices"));
            }
            has_alt = true;
        }
    }

    Ok(ParsedMessage {
        keys,
        num_required_sigs,
        num_readonly_signed,
        num_readonly_unsigned,
        has_alt,
        ixs,
    })
}

// ── Verifier ──────────────────────────────────────────────────────────────

fn verify_parsed(msg: &ParsedMessage, expected: &ExpectedSwap) -> Result<()> {
    // Owner must appear among the signers (account-keys range
    // `[0..num_required_sigs)`), but is NOT required to be the fee payer at
    // index 0. Jupiter Z (RFQ) puts the market maker at index 0 as fee payer,
    // and Ultra gasless adds Jupiter as a secondary fee payer — in both cases
    // the user is a signer at index 1+. We still require that the user signs
    // the transaction (anything weaker would let an attacker forge a tx the
    // user didn't authorize), and the input-transfer authority check below
    // independently confirms the owner authorized the actual debit.
    let n_signers = (msg.num_required_sigs as usize).min(msg.keys.len());
    if n_signers == 0 {
        return Err(VerifyError::MalformedTransaction("no signers".into()).into());
    }
    let owner_is_signer = msg.keys[..n_signers]
        .iter()
        .any(|k| k == &expected.owner_pubkey);
    if !owner_is_signer {
        return Err(VerifyError::OwnerMismatch.into());
    }

    // Pre-derive the input/output ATAs for both classic Token and Token-2022.
    let input_atas = [
        derive_ata_pubkey(&expected.owner_pubkey, &expected.input_mint, &TOKEN_PROGRAM_ID)?,
        derive_ata_pubkey(&expected.owner_pubkey, &expected.input_mint, &TOKEN_2022_PROGRAM_ID)?,
    ];
    let output_atas = [
        derive_ata_pubkey(&expected.owner_pubkey, &expected.output_mint, &TOKEN_PROGRAM_ID)?,
        derive_ata_pubkey(&expected.owner_pubkey, &expected.output_mint, &TOKEN_2022_PROGRAM_ID)?,
    ];

    // Walk instructions: find the SPL token transfer that debits one of our
    // input-mint ATAs. Whichever token-program ix references our input ATA as
    // source decides the input leg.
    let mut input_seen = false;
    let mut input_amount_mismatch: Option<(u64, u64)> = None;
    let mut input_mint_seen_other = false;

    for ix in &msg.ixs {
        let Some(prog) = msg.program_id(ix) else { continue };
        let is_token = prog == &TOKEN_PROGRAM_ID || prog == &TOKEN_2022_PROGRAM_ID;
        if !is_token {
            continue;
        }
        // Try Transfer (3) and TransferChecked (12).
        let parsed = parse_token_transfer(ix);
        let Some(t) = parsed else { continue };

        let Some(source_key) = msg.account_key(t.source_idx) else { continue };
        let Some(authority_key) = msg.account_key(t.authority_idx) else { continue };

        // Is this transfer from one of OUR ATAs?
        if input_atas.iter().any(|a| a == source_key) {
            // It must also be authorized by our wallet.
            if authority_key != &expected.owner_pubkey {
                return Err(VerifyError::OwnerMismatch.into());
            }
            if t.amount != expected.input_amount {
                input_amount_mismatch = Some((expected.input_amount, t.amount));
            } else {
                input_seen = true;
            }
            continue;
        }

        // Source is some other ATA owned by us → wrong mint
        // (Heuristic: an ATA whose authority is our pubkey but mint differs.)
        if authority_key == &expected.owner_pubkey {
            input_mint_seen_other = true;
        }
    }

    if !input_seen {
        if let Some((expected_amt, found_amt)) = input_amount_mismatch {
            return Err(VerifyError::InputAmountMismatch {
                expected: expected_amt,
                found: found_amt,
            }
            .into());
        }
        if input_mint_seen_other {
            return Err(VerifyError::InputMintMismatch.into());
        }
        return Err(VerifyError::MissingInputTransfer.into());
    }

    // Output ATA must appear as a writable static account. (If ALTs are in
    // play, we additionally accept "any output ATA referenced anywhere" since
    // Jupiter sometimes places the output ATA in a lookup table for hot pools.)
    let output_static_writable = msg
        .writable_static_indices()
        .filter_map(|i| msg.keys.get(i))
        .any(|k| output_atas.iter().any(|a| a == k));
    if output_static_writable {
        return Ok(());
    }
    if msg.has_alt {
        // LIMITATION: when ALTs are present we cannot resolve table-only writable
        // accounts without an RPC fetch. We fall back to a soft check — confirm
        // the output mint pubkey itself is referenced somewhere in the static
        // keys. A tx that hides BOTH the output ATA and the output mint behind
        // ALT lookups would slip past this layer. Defense-in-depth via on-chain
        // simulation is tracked for v0.3 if Jupiter ever ships ALT-only references.
        let output_mint_seen = msg.keys.iter().any(|k| k == &expected.output_mint);
        if output_mint_seen {
            return Ok(());
        }
    }
    Err(VerifyError::OutputAtaMissing.into())
}

#[derive(Debug)]
struct TokenTransfer {
    source_idx: u8,
    authority_idx: u8,
    amount: u64,
}

/// Parse SPL token `Transfer` (discriminator 3) or `TransferChecked` (12).
/// Returns `None` for any other ix (Approve, Burn, etc.) so the caller skips it.
fn parse_token_transfer(ix: &ParsedInstruction) -> Option<TokenTransfer> {
    let disc = *ix.data.first()?;
    match disc {
        3 => {
            // Transfer: data = [3, amount_le u64]; accounts = [source, dest, authority, ...]
            if ix.data.len() < 1 + 8 || ix.account_indices.len() < 3 {
                return None;
            }
            let amount = u64::from_le_bytes(ix.data[1..9].try_into().ok()?);
            Some(TokenTransfer {
                source_idx: ix.account_indices[0],
                authority_idx: ix.account_indices[2],
                amount,
            })
        }
        12 => {
            // TransferChecked: data = [12, amount_le u64, decimals u8]; accounts = [source, mint, dest, authority, ...]
            if ix.data.len() < 1 + 8 + 1 || ix.account_indices.len() < 4 {
                return None;
            }
            let amount = u64::from_le_bytes(ix.data[1..9].try_into().ok()?);
            Some(TokenTransfer {
                source_idx: ix.account_indices[0],
                authority_idx: ix.account_indices[3],
                amount,
            })
        }
        _ => None,
    }
}

// ── compact-u16 ───────────────────────────────────────────────────────────

fn decode_compact_u16(data: &[u8]) -> Result<(u16, usize)> {
    if data.is_empty() {
        return Err(eyre!("empty compact-u16"));
    }
    let b0 = data[0] as u16;
    if b0 < 0x80 {
        return Ok((b0, 1));
    }
    if data.len() < 2 {
        return Err(eyre!("truncated compact-u16"));
    }
    let b1 = data[1] as u16;
    if b1 < 0x80 {
        return Ok(((b0 & 0x7f) | (b1 << 7), 2));
    }
    if data.len() < 3 {
        return Err(eyre!("truncated compact-u16"));
    }
    let b2 = data[2] as u16;
    Ok(((b0 & 0x7f) | ((b1 & 0x7f) << 7) | (b2 << 14), 3))
}

fn decode_b58_32(s: &str, label: &str) -> Result<[u8; 32]> {
    let v = bs58::decode(s)
        .into_vec()
        .map_err(|e| eyre!("invalid base58 {label}: {e}"))?;
    let arr: [u8; 32] = v.as_slice().try_into()
        .map_err(|_| eyre!("invalid {label} length: expected 32 bytes"))?;
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── compact-u16 helpers (mirrors the wallet decoder) ──────────────────

    fn encode_compact_u16(val: u16, out: &mut Vec<u8>) {
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

    /// Build a synthetic v0 Jupiter-like swap transaction.
    ///
    /// Layout matches what the real Jupiter `/order` endpoint returns at the
    /// byte level: 1 sig slot, V0 tag, header, account keys (signer first),
    /// blockhash, instructions, no ALTs (extra ALTs are tested separately).
    struct FixtureBuilder {
        owner: [u8; 32],
        input_mint: [u8; 32],
        output_mint: [u8; 32],
        input_amount: u64,
        /// Override input ATA owner — used to simulate "wrong recipient" tx.
        input_ata_authority: [u8; 32],
        /// Override input mint used for ATA derivation — used to simulate "wrong input mint".
        input_ata_mint: [u8; 32],
        /// Override output mint used for ATA derivation — used to simulate "wrong output mint".
        output_ata_mint: [u8; 32],
        /// Override the transferred amount.
        forced_amount: Option<u64>,
    }

    impl FixtureBuilder {
        fn new() -> Self {
            // Deterministic fake pubkeys.
            Self {
                owner: filled(0x11),
                input_mint: filled(0x22),
                output_mint: filled(0x33),
                input_amount: 1_000_000,
                input_ata_authority: filled(0x11),
                input_ata_mint: filled(0x22),
                output_ata_mint: filled(0x33),
                forced_amount: None,
            }
        }

        fn build(&self) -> (String, ExpectedSwap) {
            let input_ata = derive_ata_pubkey(
                &self.input_ata_authority,
                &self.input_ata_mint,
                &TOKEN_PROGRAM_ID,
            )
            .unwrap();
            let output_ata = derive_ata_pubkey(
                &self.owner,
                &self.output_ata_mint,
                &TOKEN_PROGRAM_ID,
            )
            .unwrap();
            let intermediate_dest = filled(0x44);
            let blockhash = filled(0x55);

            // Account keys (static) — order chosen so that the writable layout is:
            //   [0] owner   (signer, writable)
            //   [1] input_ata  (writable, unsigned)
            //   [2] intermediate_dest (writable, unsigned)
            //   [3] output_ata (writable, unsigned)
            //   [4] token_program (readonly, unsigned)
            //   [5] mint (readonly, unsigned)  — kept here for realism
            let token_program = TOKEN_PROGRAM_ID;
            let keys: Vec<[u8; 32]> = vec![
                self.owner,
                input_ata,
                intermediate_dest,
                output_ata,
                token_program,
                self.input_mint,
            ];
            // Header: 1 required sig, 0 readonly signed, 2 readonly unsigned (token_program + mint)
            let header = (1u8, 0u8, 2u8);

            // Instruction: TransferChecked from input_ata via token_program.
            // accounts: [source(1), mint(5), dest(2), authority(0)]
            let amount = self.forced_amount.unwrap_or(self.input_amount);
            let mut ix_data = vec![12u8];
            ix_data.extend_from_slice(&amount.to_le_bytes());
            ix_data.push(6); // decimals
            let ix = ParsedInstruction {
                program_id_index: 4,
                account_indices: vec![1, 5, 2, 0],
                data: ix_data,
            };

            // Serialize the v0 message.
            let msg_bytes = serialize_v0_message(header, &keys, &blockhash, &[ix]);
            // Wrap in a transaction with 1 empty sig slot.
            let mut tx = Vec::new();
            encode_compact_u16(1, &mut tx);
            tx.extend_from_slice(&[0u8; 64]);
            tx.extend_from_slice(&msg_bytes);

            let b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

            let expected = ExpectedSwap {
                input_mint: self.input_mint,
                input_amount: self.input_amount,
                output_mint: self.output_mint,
                owner_pubkey: self.owner,
            };
            (b64, expected)
        }
    }

    fn filled(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn serialize_v0_message(
        header: (u8, u8, u8),
        keys: &[[u8; 32]],
        blockhash: &[u8; 32],
        ixs: &[ParsedInstruction],
    ) -> Vec<u8> {
        let mut out = vec![0x80, header.0, header.1, header.2];
        encode_compact_u16(keys.len() as u16, &mut out);
        for k in keys {
            out.extend_from_slice(k);
        }
        out.extend_from_slice(blockhash);
        encode_compact_u16(ixs.len() as u16, &mut out);
        for ix in ixs {
            out.push(ix.program_id_index);
            encode_compact_u16(ix.account_indices.len() as u16, &mut out);
            out.extend_from_slice(&ix.account_indices);
            encode_compact_u16(ix.data.len() as u16, &mut out);
            out.extend_from_slice(&ix.data);
        }
        // Zero address-table-lookups for V0 messages without ALT.
        encode_compact_u16(0, &mut out);
        out
    }

    fn assert_err_kind(result: Result<()>, want: VerifyError) {
        let err = result.expect_err("expected verification failure");
        let got = err
            .downcast_ref::<VerifyError>()
            .expect("VerifyError should be downcastable");
        assert_eq!(got, &want, "wrong VerifyError variant");
    }

    // ── AC-1.5 happy path ────────────────────────────────────────────────

    #[test]
    fn sign_jupiter_tx_happy_path() {
        let (b64, expected) = FixtureBuilder::new().build();
        decode_and_verify(&b64, &expected).expect("happy path should verify");
    }

    // ── AC-1.2 swapped recipient ─────────────────────────────────────────
    //
    // Attack model: Jupiter (or a MITM) returns a tx where the input transfer
    // looks legitimate from our wallet, but the writable "output ATA" belongs
    // to an attacker. The verifier must refuse because no ATA owned by *us*
    // for the expected output mint is present in the writable static set.

    #[test]
    fn sign_jupiter_tx_rejects_swapped_recipient() {
        let owner = filled(0x11);
        let input_mint = filled(0x22);
        let output_mint = filled(0x33);
        let input_amount = 1_000_000u64;
        let input_ata = derive_ata_pubkey(&owner, &input_mint, &TOKEN_PROGRAM_ID).unwrap();
        // Output ATA belongs to attacker (different authority), not us.
        let attacker = filled(0xAA);
        let attacker_output_ata = derive_ata_pubkey(&attacker, &output_mint, &TOKEN_PROGRAM_ID).unwrap();
        let intermediate = filled(0x44);
        let blockhash = filled(0x55);

        let token_program = TOKEN_PROGRAM_ID;
        let keys: Vec<[u8; 32]> = vec![
            owner, input_ata, intermediate, attacker_output_ata, token_program, input_mint,
        ];
        let header = (1u8, 0u8, 2u8);

        let mut ix_data = vec![12u8];
        ix_data.extend_from_slice(&input_amount.to_le_bytes());
        ix_data.push(6);
        let ix = ParsedInstruction {
            program_id_index: 4,
            account_indices: vec![1, 5, 2, 0],
            data: ix_data,
        };

        let msg_bytes = serialize_v0_message(header, &keys, &blockhash, &[ix]);
        let mut tx = Vec::new();
        encode_compact_u16(1, &mut tx);
        tx.extend_from_slice(&[0u8; 64]);
        tx.extend_from_slice(&msg_bytes);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

        let expected = ExpectedSwap {
            input_mint,
            input_amount,
            output_mint,
            owner_pubkey: owner,
        };
        assert_err_kind(decode_and_verify(&b64, &expected), VerifyError::OutputAtaMissing);
    }

    // ── AC-1.3 amount mismatch ───────────────────────────────────────────

    #[test]
    fn sign_jupiter_tx_rejects_amount_mismatch() {
        let mut b = FixtureBuilder::new();
        b.forced_amount = Some(b.input_amount * 2);
        let (b64, expected) = b.build();
        assert_err_kind(
            decode_and_verify(&b64, &expected),
            VerifyError::InputAmountMismatch {
                expected: 1_000_000,
                found: 2_000_000,
            },
        );
    }

    // ── AC-1.4 wrong output mint ─────────────────────────────────────────

    #[test]
    fn sign_jupiter_tx_rejects_wrong_output_mint() {
        let mut b = FixtureBuilder::new();
        // Tx routes to ATA(owner, attacker_mint) — but expected says output_mint=0x33.
        b.output_ata_mint = filled(0x99);
        let (b64, expected) = b.build();
        assert_err_kind(decode_and_verify(&b64, &expected), VerifyError::OutputAtaMissing);
    }

    // ── Wrong input mint ─────────────────────────────────────────────────

    #[test]
    fn sign_jupiter_tx_rejects_wrong_input_mint() {
        let mut b = FixtureBuilder::new();
        // ATA debited belongs to us but for a *different* mint than expected.
        b.input_ata_mint = filled(0x99);
        let (b64, expected) = b.build();
        assert_err_kind(decode_and_verify(&b64, &expected), VerifyError::InputMintMismatch);
    }

    // ── Owner mismatch (we are not the fee payer) ────────────────────────

    #[test]
    fn sign_jupiter_tx_rejects_owner_mismatch() {
        let mut b = FixtureBuilder::new();
        b.owner = filled(0x11);
        let (b64, mut expected) = b.build();
        expected.owner_pubkey = filled(0x77); // we are someone else
        assert_err_kind(decode_and_verify(&b64, &expected), VerifyError::OwnerMismatch);
    }

    // ── Malformed input ──────────────────────────────────────────────────

    #[test]
    fn malformed_base64_returns_malformed() {
        let expected = ExpectedSwap {
            input_mint: filled(1),
            input_amount: 1,
            output_mint: filled(2),
            owner_pubkey: filled(3),
        };
        let err = decode_and_verify("!!!notbase64!!!", &expected).unwrap_err();
        let got = err.downcast_ref::<VerifyError>().unwrap();
        assert!(matches!(got, VerifyError::MalformedTransaction(_)));
    }

    #[test]
    fn truncated_tx_returns_malformed() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0x01u8, 0x02, 0x03]);
        let expected = ExpectedSwap {
            input_mint: filled(1),
            input_amount: 1,
            output_mint: filled(2),
            owner_pubkey: filled(3),
        };
        let err = decode_and_verify(&b64, &expected).unwrap_err();
        let got = err.downcast_ref::<VerifyError>().unwrap();
        assert!(matches!(got, VerifyError::MalformedTransaction(_)));
    }

    // ── ExpectedSwap::from_base58 ────────────────────────────────────────

    #[test]
    fn expected_swap_from_base58_roundtrips() {
        let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let owner = "5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47";
        let out_mint = "So11111111111111111111111111111111111111112";
        let exp = ExpectedSwap::from_base58(usdc, 12345, out_mint, owner).unwrap();
        assert_eq!(exp.input_amount, 12345);
        assert_eq!(exp.input_mint.len(), 32);
        assert_eq!(exp.output_mint.len(), 32);
    }

    // ── Tolerance: extra unknown instructions are allowed ────────────────

    #[test]
    fn tolerates_extra_compute_budget_instructions() {
        // Build the standard happy fixture, then prepend a compute-budget-like
        // instruction by reconstructing the message manually.
        let owner = filled(0x11);
        let input_mint = filled(0x22);
        let output_mint = filled(0x33);
        let input_amount = 1_000_000u64;
        let input_ata = derive_ata_pubkey(&owner, &input_mint, &TOKEN_PROGRAM_ID).unwrap();
        let output_ata = derive_ata_pubkey(&owner, &output_mint, &TOKEN_PROGRAM_ID).unwrap();
        let blockhash = filled(0x55);
        let intermediate = filled(0x44);
        // 7 keys: owner, input_ata, intermediate, output_ata, token_program, mint, compute_budget
        let cb_program = filled(0xCB);
        let token_program = TOKEN_PROGRAM_ID;
        let keys: Vec<[u8; 32]> = vec![
            owner, input_ata, intermediate, output_ata, token_program, input_mint, cb_program,
        ];
        let header = (1u8, 0u8, 3u8); // 3 readonly unsigned: token_program, mint, compute_budget

        // Instruction 0: a "compute budget" set-unit-limit (data: [2, units_le u32])
        let cb_ix = ParsedInstruction {
            program_id_index: 6,
            account_indices: vec![],
            data: {
                let mut d = vec![2u8];
                d.extend_from_slice(&200_000u32.to_le_bytes());
                d
            },
        };
        // Instruction 1: TransferChecked debiting input_ata.
        let mut ix_data = vec![12u8];
        ix_data.extend_from_slice(&input_amount.to_le_bytes());
        ix_data.push(6);
        let transfer_ix = ParsedInstruction {
            program_id_index: 4,
            account_indices: vec![1, 5, 2, 0],
            data: ix_data,
        };

        let msg_bytes = serialize_v0_message(header, &keys, &blockhash, &[cb_ix, transfer_ix]);
        let mut tx = Vec::new();
        encode_compact_u16(1, &mut tx);
        tx.extend_from_slice(&[0u8; 64]);
        tx.extend_from_slice(&msg_bytes);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

        let expected = ExpectedSwap {
            input_mint,
            input_amount,
            output_mint,
            owner_pubkey: owner,
        };
        decode_and_verify(&b64, &expected).expect("extra unknown ix should be allowed");
    }

    // ── Gasless flow: external fee payer at index 0 ──────────────────────
    //
    // Jupiter Z (RFQ) and Ultra gasless make a market maker / Jupiter the fee
    // payer at signer index 0; the user is a secondary signer at index 1.
    // The verifier must accept this as long as the user is among the signers
    // and authorizes the input transfer.

    #[test]
    fn accepts_gasless_with_external_fee_payer() {
        let mm = filled(0xEE); // market maker / Jupiter, pays fees
        let owner = filled(0x11);
        let input_mint = filled(0x22);
        let output_mint = filled(0x33);
        let input_amount = 1_000_000u64;
        let input_ata = derive_ata_pubkey(&owner, &input_mint, &TOKEN_PROGRAM_ID).unwrap();
        let output_ata = derive_ata_pubkey(&owner, &output_mint, &TOKEN_PROGRAM_ID).unwrap();
        let intermediate = filled(0x44);
        let blockhash = filled(0x55);
        let token_program = TOKEN_PROGRAM_ID;

        // Account-keys layout:
        //   [0] mm           (signer, writable, fee payer)
        //   [1] owner        (signer, writable)
        //   [2] input_ata    (writable, unsigned)
        //   [3] intermediate (writable, unsigned)
        //   [4] output_ata   (writable, unsigned)
        //   [5] token_program (readonly, unsigned)
        //   [6] mint         (readonly, unsigned)
        let keys: Vec<[u8; 32]> = vec![
            mm, owner, input_ata, intermediate, output_ata, token_program, input_mint,
        ];
        // Header: 2 required sigs (mm + owner), 0 readonly signed, 2 readonly unsigned.
        let header = (2u8, 0u8, 2u8);

        // TransferChecked: source=input_ata(2), mint(6), dest=intermediate(3), authority=owner(1)
        let mut ix_data = vec![12u8];
        ix_data.extend_from_slice(&input_amount.to_le_bytes());
        ix_data.push(6);
        let ix = ParsedInstruction {
            program_id_index: 5,
            account_indices: vec![2, 6, 3, 1],
            data: ix_data,
        };

        let msg_bytes = serialize_v0_message(header, &keys, &blockhash, &[ix]);
        let mut tx = Vec::new();
        encode_compact_u16(2, &mut tx); // 2 sig slots — mm and owner
        tx.extend_from_slice(&[0u8; 128]);
        tx.extend_from_slice(&msg_bytes);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

        let expected = ExpectedSwap {
            input_mint,
            input_amount,
            output_mint,
            owner_pubkey: owner,
        };
        decode_and_verify(&b64, &expected)
            .expect("gasless tx with mm as fee payer should verify");
    }

    // ── Gasless: still rejects when owner is not in signer set ───────────
    //
    // Even with a valid fee payer at index 0, if `owner_pubkey` does not
    // appear among signers `[0..num_required_sigs)` we must refuse — that
    // would mean the user never signed the transaction at all.

    #[test]
    fn gasless_rejects_when_owner_not_signer() {
        let mm = filled(0xEE);
        let attacker = filled(0xAA); // pretends to be a signer
        let real_owner = filled(0x11); // expected, but absent from signer set
        let input_mint = filled(0x22);
        let output_mint = filled(0x33);
        let input_amount = 1_000_000u64;
        // Attacker constructs a tx where input ATA is theirs (not real_owner's)
        // and tries to pass the user pubkey via ExpectedSwap.
        let input_ata = derive_ata_pubkey(&attacker, &input_mint, &TOKEN_PROGRAM_ID).unwrap();
        let output_ata = derive_ata_pubkey(&attacker, &output_mint, &TOKEN_PROGRAM_ID).unwrap();
        let intermediate = filled(0x44);
        let blockhash = filled(0x55);
        let token_program = TOKEN_PROGRAM_ID;

        // Signers: mm (fee payer, idx 0) + attacker (idx 1). real_owner is NOT here.
        let keys: Vec<[u8; 32]> =
            vec![mm, attacker, input_ata, intermediate, output_ata, token_program, input_mint];
        let header = (2u8, 0u8, 2u8);

        let mut ix_data = vec![12u8];
        ix_data.extend_from_slice(&input_amount.to_le_bytes());
        ix_data.push(6);
        let ix = ParsedInstruction {
            program_id_index: 5,
            account_indices: vec![2, 6, 3, 1],
            data: ix_data,
        };

        let msg_bytes = serialize_v0_message(header, &keys, &blockhash, &[ix]);
        let mut tx = Vec::new();
        encode_compact_u16(2, &mut tx);
        tx.extend_from_slice(&[0u8; 128]);
        tx.extend_from_slice(&msg_bytes);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

        let expected = ExpectedSwap {
            input_mint,
            input_amount,
            output_mint,
            owner_pubkey: real_owner,
        };
        assert_err_kind(decode_and_verify(&b64, &expected), VerifyError::OwnerMismatch);
    }

    // ── compact-u16 ──────────────────────────────────────────────────────

    #[test]
    fn compact_u16_roundtrip() {
        for v in [0u16, 1, 127, 128, 255, 16383, 16384, 65535] {
            let mut buf = Vec::new();
            encode_compact_u16(v, &mut buf);
            let (got, _) = decode_compact_u16(&buf).unwrap();
            assert_eq!(got, v);
        }
    }
}
