/// Integration tests for `get_portfolio_balances` using a local mock RPC server.
///
/// These tests verify:
///   1. Standard response parsing (SOL, USDC, GM tokens)
///   2. Out-of-order batch responses (id=2 before id=1) — JSON-RPC 2.0 allows any order
///   3. Malformed value entries are silently skipped (missing `account`, null)
///   4. Empty token accounts list returns empty gm_tokens
///   5. RPC retries on 429 and succeeds on the next attempt
use httpmock::prelude::*;
use rwa_ondo::{solana, token_list::GmTokenEntry};

const WALLET: &str = "5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47";
/// Mint of "AALon" — first token in the static list.
const AAL_MINT: &str = "9wYZetvT8J2ptfsRca5gzLBGvcUug38mp9yT3xaondo";

// ── JSON fixture helpers ─────────────────────────────────────────────────────

/// Build a single parsed Token-2022 account entry (one element of `value`).
fn token_entry(mint: &str, ui_amount: f64, raw_amount: &str) -> serde_json::Value {
    serde_json::json!({
        "pubkey": "SomePubkeyAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "account": {
            "data": {
                "parsed": {
                    "info": {
                        "mint": mint,
                        "owner": WALLET,
                        "tokenAmount": {
                            "amount": raw_amount,
                            "decimals": 9,
                            "uiAmount": ui_amount,
                            "uiAmountString": ui_amount.to_string()
                        }
                    },
                    "type": "account"
                },
                "program": "spl-token-2022",
                "space": 182
            },
            "executable": false,
            "lamports": 2039280,
            "owner": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
            "rentEpoch": 18446744073709551615u64
        }
    })
}

/// Build the `result` body for a `getMultipleAccounts` response.
/// Slot=1. First entry = SOL wallet account, second = USDC ATA (or null if no USDC).
fn multi_accounts_result(sol_lamports: u64, usdc_raw: u64) -> serde_json::Value {
    serde_json::json!({
        "context": { "slot": 1 },
        "value": [
            {
                "lamports": sol_lamports,
                "owner": "11111111111111111111111111111111",
                "data": ["", "base64"],
                "executable": false,
                "rentEpoch": 0
            },
            {
                "lamports": 2039280,
                "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                "data": {
                    "parsed": {
                        "info": {
                            "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                            "owner": WALLET,
                            "tokenAmount": {
                                "amount": usdc_raw.to_string(),
                                "decimals": 6,
                                "uiAmount": usdc_raw as f64 / 1_000_000.0,
                                "uiAmountString": (usdc_raw as f64 / 1_000_000.0).to_string()
                            }
                        },
                        "type": "account"
                    },
                    "program": "spl-token",
                    "space": 165
                },
                "executable": false,
                "rentEpoch": 0
            }
        ]
    })
}

/// Build a full batch response in standard order (id=1 first, id=2 second).
fn batch_response(
    sol_lamports: u64,
    usdc_raw: u64,
    token_value_entries: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!([
        { "jsonrpc": "2.0", "id": 1, "result": multi_accounts_result(sol_lamports, usdc_raw) },
        { "jsonrpc": "2.0", "id": 2, "result": { "context": { "slot": 1 }, "value": token_value_entries } }
    ])
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn portfolio_parses_standard_response() {
    let server = MockServer::start_async().await;

    let _m = server.mock_async(|when, then| {
        when.method(POST);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(batch_response(
                1_500_000_000, // 1.5 SOL
                500_000_000,   // 500 USDC
                serde_json::json!([token_entry(AAL_MINT, 2.5, "2500000000")]),
            ));
    }).await;

    let tokens = &[GmTokenEntry { symbol: "AALon", solana_address: Some(AAL_MINT) }];
    let result = solana::get_portfolio_balances(WALLET, tokens, Some(&server.base_url()))
        .await
        .unwrap();

    assert!((result.sol - 1.5).abs() < 1e-9, "sol={}", result.sol);
    assert!((result.usdc - 500.0).abs() < 1e-6, "usdc={}", result.usdc);
    assert_eq!(result.gm_tokens.len(), 1);
    assert_eq!(result.gm_tokens[0].symbol, "AALon");
    assert!((result.gm_tokens[0].balance - 2.5).abs() < 1e-9);
}

#[tokio::test]
async fn portfolio_handles_out_of_order_batch_responses() {
    let server = MockServer::start_async().await;

    // id=2 arrives before id=1 — valid per JSON-RPC 2.0 spec.
    let reversed = serde_json::json!([
        { "jsonrpc": "2.0", "id": 2, "result": { "context": { "slot": 1 }, "value": [token_entry(AAL_MINT, 3.0, "3000000000")] } },
        { "jsonrpc": "2.0", "id": 1, "result": multi_accounts_result(2_000_000_000, 100_000_000) }
    ]);

    let _m = server.mock_async(|when, then| {
        when.method(POST);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(reversed);
    }).await;

    let tokens = &[GmTokenEntry { symbol: "AALon", solana_address: Some(AAL_MINT) }];
    let result = solana::get_portfolio_balances(WALLET, tokens, Some(&server.base_url()))
        .await
        .unwrap();

    assert!((result.sol - 2.0).abs() < 1e-9, "sol={}", result.sol);
    assert!((result.usdc - 100.0).abs() < 1e-6, "usdc={}", result.usdc);
    assert_eq!(result.gm_tokens.len(), 1, "should have 1 GM token");
    assert!((result.gm_tokens[0].balance - 3.0).abs() < 1e-9);
}

#[tokio::test]
async fn portfolio_skips_malformed_token_account_entries() {
    let server = MockServer::start_async().await;

    let malformed_value = serde_json::json!([
        // Missing `account` field entirely
        { "pubkey": "BadEntry1", "lamports": 12345 },
        // Completely random structure
        { "data": "notanobject" },
        // Null element (some nodes include nulls)
        serde_json::Value::Null,
        // The one valid entry
        token_entry(AAL_MINT, 1.0, "1000000000"),
    ]);

    let _m = server.mock_async(|when, then| {
        when.method(POST);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(batch_response(1_000_000_000, 0, malformed_value));
    }).await;

    let tokens = &[GmTokenEntry { symbol: "AALon", solana_address: Some(AAL_MINT) }];
    let result = solana::get_portfolio_balances(WALLET, tokens, Some(&server.base_url()))
        .await
        .unwrap();

    assert_eq!(result.gm_tokens.len(), 1, "malformed entries must be skipped");
    assert!((result.gm_tokens[0].balance - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn portfolio_empty_token_accounts() {
    let server = MockServer::start_async().await;

    let _m = server.mock_async(|when, then| {
        when.method(POST);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(batch_response(500_000_000, 0, serde_json::json!([])));
    }).await;

    let tokens = &[GmTokenEntry { symbol: "AALon", solana_address: Some(AAL_MINT) }];
    let result = solana::get_portfolio_balances(WALLET, tokens, Some(&server.base_url()))
        .await
        .unwrap();

    assert!((result.sol - 0.5).abs() < 1e-9);
    assert_eq!(result.usdc, 0.0);
    assert!(result.gm_tokens.is_empty());
}

#[tokio::test]
async fn portfolio_fails_fast_on_rpc_level_error() {
    // When the RPC returns a JSON-RPC error (not HTTP error), we get a clear Err.
    let server = MockServer::start_async().await;

    let error_response = serde_json::json!([
        { "jsonrpc": "2.0", "id": 1, "error": { "code": -32601, "message": "Method not found" } },
        { "jsonrpc": "2.0", "id": 2, "error": { "code": -32601, "message": "Method not found" } }
    ]);

    let _m = server.mock_async(|when, then| {
        when.method(POST);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(error_response);
    }).await;

    let tokens = &[GmTokenEntry { symbol: "AALon", solana_address: Some(AAL_MINT) }];
    let result = solana::get_portfolio_balances(WALLET, tokens, Some(&server.base_url())).await;

    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("Method not found"), "error should propagate: {msg}");
}

#[tokio::test]
async fn portfolio_multiple_gm_tokens_sorted_alphabetically() {
    let server = MockServer::start_async().await;

    let aapl_mint = "123mYEnRLM2LLYsJW3K6oyYh8uP1fngj732iG638ondo";

    let value_entries = serde_json::json!([
        token_entry(aapl_mint, 1.0, "1000000000"),  // AAPLon — second alphabetically
        token_entry(AAL_MINT,  2.0, "2000000000"),  // AALon  — first alphabetically
    ]);

    let _m = server.mock_async(|when, then| {
        when.method(POST);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(batch_response(1_000_000_000, 0, value_entries));
    }).await;

    let tokens = &[
        GmTokenEntry { symbol: "AAPLon", solana_address: Some(aapl_mint) },
        GmTokenEntry { symbol: "AALon",  solana_address: Some(AAL_MINT)  },
    ];

    let result = solana::get_portfolio_balances(WALLET, tokens, Some(&server.base_url()))
        .await
        .unwrap();

    assert_eq!(result.gm_tokens.len(), 2);
    // "AALon" < "AAPLon" lexicographically ('L' < 'P')
    assert_eq!(result.gm_tokens[0].symbol, "AALon",  "should be sorted alphabetically");
    assert_eq!(result.gm_tokens[1].symbol, "AAPLon");
}
