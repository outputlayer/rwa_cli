//! Byte-level parser for Solana v0/legacy transaction messages.
//!
//! Extracted from the sign-time verifier (`wallet::verify`) so the parsing
//! layer can be tested in isolation — including against malformed and
//! truncated bytes — without involving any verification policy. This module
//! contains NO decision logic: it only turns raw transaction bytes into a
//! [`ParsedMessage`] or a descriptive error.

use eyre::{Result, eyre};

/// What we managed to extract from the v0/legacy message.
#[derive(Debug)]
pub(crate) struct ParsedMessage {
    /// Static account keys, in message order. (Address-table-lookup keys are
    /// appended after these but we don't know their values without the table —
    /// for verification we only rely on static keys; ALT-only writes never
    /// carry the user's ATA.)
    pub(crate) keys: Vec<[u8; 32]>,
    /// `keys` index ranges that are signer / writable, derived from the header.
    pub(crate) num_required_sigs: u8,
    pub(crate) num_readonly_signed: u8,
    pub(crate) num_readonly_unsigned: u8,
    /// True if ALT lookups are present — extra writable/readonly account slots.
    /// We don't resolve them, but the verifier treats `OutputAtaMissing` more
    /// leniently when ALTs are in play.
    pub(crate) has_alt: bool,
    /// Instructions in declared order.
    pub(crate) ixs: Vec<ParsedInstruction>,
}

#[derive(Debug)]
pub(crate) struct ParsedInstruction {
    pub(crate) program_id_index: u8,
    pub(crate) account_indices: Vec<u8>,
    pub(crate) data: Vec<u8>,
}

impl ParsedMessage {
    pub(crate) fn program_id(&self, ix: &ParsedInstruction) -> Option<&[u8; 32]> {
        self.keys.get(ix.program_id_index as usize)
    }
    pub(crate) fn account_key(&self, idx: u8) -> Option<&[u8; 32]> {
        self.keys.get(idx as usize)
    }
    pub(crate) fn writable_static_indices(&self) -> impl Iterator<Item = usize> + '_ {
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

pub(crate) fn parse_message(tx_bytes: &[u8]) -> Result<ParsedMessage> {
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

    // Audit L2: the header's `num_required_sigs` must never exceed the
    // outer signature-slot count decoded above — a crafted message could
    // otherwise claim more required signers than real signature slots
    // exist, which downstream signer-slot lookups (`Wallet::sign_transaction`)
    // must not have to defend against on their own. Reject the shape here,
    // up front, rather than trusting individual callers to clamp correctly.
    if num_required_sigs as usize > num_sigs as usize {
        return Err(eyre!(
            "message header claims {num_required_sigs} required signers but only {num_sigs} signature slot(s) exist"
        ));
    }

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

// ── compact-u16 ───────────────────────────────────────────────────────────

pub(crate) fn decode_compact_u16(data: &[u8]) -> Result<(u16, usize)> {
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

// ── Test fixtures shared with the verifier's tests ────────────────────────

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::ParsedInstruction;

    pub(crate) fn encode_compact_u16(val: u16, out: &mut Vec<u8>) {
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

    pub(crate) fn serialize_v0_message(
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

    /// A minimal valid v0 transaction: 1 sig slot, 2 keys, 1 instruction.
    pub(crate) fn minimal_valid_tx() -> Vec<u8> {
        let keys = [[0x11u8; 32], [0x22u8; 32]];
        let ix = ParsedInstruction {
            program_id_index: 1,
            account_indices: vec![0],
            data: vec![1, 2, 3],
        };
        let msg = serialize_v0_message((1, 0, 1), &keys, &[0x55u8; 32], &[ix]);
        let mut tx = Vec::new();
        encode_compact_u16(1, &mut tx);
        tx.extend_from_slice(&[0u8; 64]);
        tx.extend_from_slice(&msg);
        tx
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::*;
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn compact_u16_roundtrip() {
        for v in [0u16, 1, 127, 128, 255, 16383, 16384, 65535] {
            let mut buf = Vec::new();
            encode_compact_u16(v, &mut buf);
            let (got, _) = decode_compact_u16(&buf).unwrap();
            assert_eq!(got, v);
        }
    }

    #[test]
    fn parses_minimal_valid_v0_message() {
        let tx = minimal_valid_tx();
        let parsed = parse_message(&tx).expect("valid tx must parse");
        assert_eq!(parsed.keys.len(), 2);
        assert_eq!(parsed.num_required_sigs, 1);
        assert_eq!(parsed.num_readonly_unsigned, 1);
        assert!(!parsed.has_alt);
        assert_eq!(parsed.ixs.len(), 1);
        assert_eq!(parsed.ixs[0].program_id_index, 1);
        assert_eq!(parsed.ixs[0].account_indices, vec![0]);
        assert_eq!(parsed.ixs[0].data, vec![1, 2, 3]);
    }

    #[test]
    fn every_truncation_of_a_valid_tx_errors_not_panics() {
        let tx = minimal_valid_tx();
        for len in 0..tx.len() {
            // Must never panic. Every truncation must fail to parse, except
            // dropping only the trailing ALT-lookup count: the parser treats
            // the ALT section as optional (documented tolerance — the verifier
            // never trusts ALT-resolved keys anyway).
            let res = parse_message(&tx[..len]);
            if len == tx.len() - 1 {
                assert!(res.is_ok(), "missing ALT count alone is tolerated");
            } else {
                assert!(res.is_err(), "truncated tx of len {len} should fail to parse");
            }
        }
    }

    proptest! {
        /// The parser must never panic on arbitrary bytes — a malicious quote
        /// endpoint controls this input entirely.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_message(&bytes);
        }

        /// Flipping any single byte of a valid tx must not panic.
        #[test]
        fn single_byte_corruption_never_panics(idx in 0usize..150, val in any::<u8>()) {
            let mut tx = minimal_valid_tx();
            if idx < tx.len() {
                tx[idx] = val;
            }
            let _ = parse_message(&tx);
        }
    }
}
