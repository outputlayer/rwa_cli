//! Microbenchmarks for hot, CPU-bound, network-free paths.
//!
//! Run: `cargo bench -p rwa-ondo`
//!
//! These guard against regressions in the pure compute that runs on every
//! command: amount formatting (rendered for every balance/quote), address
//! validation (every transfer target), and — most importantly — the sign-time
//! transaction byte-parser `decode_and_verify`, which runs before every swap.

use base64::Engine;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

use rwa_ondo::amounts;
use rwa_ondo::solana::{self, TOKEN_PROGRAM_ID, derive_ata_pubkey};
use rwa_ondo::wallet::{ExpectedSwap, decode_and_verify};

fn encode_cu16(v: u16, out: &mut Vec<u8>) {
    if v < 0x80 {
        out.push(v as u8);
    } else if v < 0x4000 {
        out.push((v & 0x7f) as u8 | 0x80);
        out.push((v >> 7) as u8);
    } else {
        out.push((v & 0x7f) as u8 | 0x80);
        out.push(((v >> 7) & 0x7f) as u8 | 0x80);
        out.push((v >> 14) as u8);
    }
}

/// Build a realistic Jupiter-shaped v0 swap transaction (one TransferChecked
/// debiting the owner's input ATA), matching the byte layout the parser sees.
fn sample_swap_tx() -> (String, ExpectedSwap) {
    let owner = [0x11u8; 32];
    let input_mint = [0x22u8; 32];
    let output_mint = [0x33u8; 32];
    let amount = 1_000_000u64;
    let input_ata = derive_ata_pubkey(&owner, &input_mint, &TOKEN_PROGRAM_ID).unwrap();
    let output_ata = derive_ata_pubkey(&owner, &output_mint, &TOKEN_PROGRAM_ID).unwrap();
    let intermediate = [0x44u8; 32];
    let blockhash = [0x55u8; 32];

    // Static keys: [owner, input_ata, intermediate, output_ata, token_program, input_mint]
    let keys: Vec<[u8; 32]> = vec![
        owner,
        input_ata,
        intermediate,
        output_ata,
        TOKEN_PROGRAM_ID,
        input_mint,
    ];

    // v0 message: tag(0x80) + header(1 sig, 0 ro-signed, 2 ro-unsigned)
    let mut msg = vec![0x80u8, 1, 0, 2];
    encode_cu16(keys.len() as u16, &mut msg);
    for k in &keys {
        msg.extend_from_slice(k);
    }
    msg.extend_from_slice(&blockhash);

    // 1 instruction: TransferChecked, accounts [source=1, mint=5, dest=2, authority=0]
    encode_cu16(1, &mut msg);
    msg.push(4); // program id index → token_program
    encode_cu16(4, &mut msg);
    msg.extend_from_slice(&[1, 5, 2, 0]);
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(6); // decimals
    encode_cu16(data.len() as u16, &mut msg);
    msg.extend_from_slice(&data);
    encode_cu16(0, &mut msg); // 0 address-table lookups

    // Wrap in a transaction with 1 (empty) signature slot.
    let mut tx = Vec::new();
    encode_cu16(1, &mut tx);
    tx.extend_from_slice(&[0u8; 64]);
    tx.extend_from_slice(&msg);

    let b64 = base64::engine::general_purpose::STANDARD.encode(&tx);
    let expected = ExpectedSwap {
        input_mint,
        input_amount: amount,
        output_mint,
        owner_pubkey: owner,
    };
    (b64, expected)
}

fn benches(c: &mut Criterion) {
    c.bench_function("format_amount", |b| {
        b.iter(|| amounts::format_amount(black_box("123456789012345"), black_box(9)))
    });

    c.bench_function("validate_address", |b| {
        b.iter(|| solana::validate_address(black_box("5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47")))
    });

    // The sign-time guard's static parser — runs before every swap.
    let (tx, expected) = sample_swap_tx();
    // Sanity: the fixture must verify, or we'd be benchmarking the error path.
    assert!(decode_and_verify(&tx, &expected).is_ok());
    c.bench_function("decode_and_verify", |b| {
        b.iter(|| decode_and_verify(black_box(&tx), black_box(&expected)))
    });
}

criterion_group!(hot_paths, benches);
criterion_main!(hot_paths);
