//! End-to-end CLI contract tests: spawn the real `rwa` binary against local
//! httpmock servers and assert the stable `--json` output shapes that agents
//! and scripts rely on (see CLAUDE.md "Agent usage rules").
//!
//! Hermetic: HOME/XDG_CONFIG_HOME point at a temp dir (own wallet, lock, cache),
//! and all external endpoints are overridden via dev env seams:
//! `RWA_RPC_URL` (public), `RWA_ONDO_API_URL`, `RWA_ONDO_SESSION_URL`,
//! `RWA_JUPITER_URL` (test-only seams).

use httpmock::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_rwa");
const WALLET: &str = "5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47";
/// Mint of "AALon" — first token in the static list.
const AAL_MINT: &str = "9wYZetvT8J2ptfsRca5gzLBGvcUug38mp9yT3xaondo";

// ── Harness ──────────────────────────────────────────────────────────────────

fn test_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rwa-cli-contract-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `rwa` command with an isolated config dir and no RWA_* leakage from the
/// developer's environment.
fn rwa(home: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("HOME", home).env("XDG_CONFIG_HOME", home.join(".config"));
    for var in [
        "RWA_RPC_URL",
        "RWA_JUPITER_API_KEY",
        "RWA_EXCLUDE_ROUTERS",
        "RWA_MAX_BPS",
        "RWA_PASSPHRASE",
        "RWA_ONDO_API_URL",
        "RWA_ONDO_SESSION_URL",
        "RWA_JUPITER_URL",
    ] {
        c.env_remove(var);
    }
    c
}

fn stdout_json(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout is not valid JSON: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

// ── RPC fixtures (same shapes as crates/ondo/tests/rpc_portfolio.rs) ────────

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

/// `gm portfolio --json` emits the nested cash/gm_positions contract, with
/// `unavailable` and `source` omitted on the happy path.
#[test]
fn portfolio_json_emits_nested_cash_and_positions_shape() {
    let home = test_home("portfolio-shape");
    let server = MockServer::start();

    let rpc = server.mock(|when, then| {
        when.method(POST).path("/rpc");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(batch_response(500_000_000, 250_000_000, serde_json::json!([])));
    });
    let assets = server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "assets": [] }));
    });

    let out = rwa(&home)
        .args(["--json", "gm", "portfolio", WALLET])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .output()
        .unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = stdout_json(&out);
    assert_eq!(v["wallet"], WALLET);
    assert_eq!(v["cash"]["sol"], 0.5);
    assert_eq!(v["cash"]["usdc"], 250.0);
    assert!(v["gm_positions"]["positions"].as_array().unwrap().is_empty());
    assert_eq!(v["gm_positions"]["value_usd"], 0.0);
    assert!(v.get("unavailable").is_none(), "empty unavailable must be omitted");
    assert!(v.get("source").is_none(), "source must be omitted on RPC path");

    rpc.assert_hits(1);
    assets.assert();
}

/// A held token with no market data lands in `unavailable[]` instead of
/// silently disappearing or failing the whole command.
#[test]
fn portfolio_json_reports_unavailable_market_data() {
    let home = test_home("portfolio-unavailable");
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(POST).path("/rpc");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(batch_response(
                1_000_000_000,
                0,
                serde_json::json!([token_entry(AAL_MINT, 2.5, "2500000000")]),
            ));
    });
    let assets = server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "assets": [] }));
    });

    let out = rwa(&home)
        .args(["--json", "gm", "portfolio", WALLET])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .output()
        .unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = stdout_json(&out);
    assert!(v["gm_positions"]["positions"].as_array().unwrap().is_empty());
    let unavailable = v["unavailable"].as_array().unwrap();
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0]["symbol"], "AALon");
    assert!(unavailable[0]["reason"].as_str().unwrap().contains("market data"));

    assets.assert();
}

/// A hard RPC failure surfaces the JSON error envelope with a stable
/// `error_kind` ("rpc_unavailable") and exit code 1.
#[test]
fn rpc_failure_emits_error_envelope_with_kind() {
    let home = test_home("rpc-error-envelope");
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(POST).path("/rpc");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!([
                { "jsonrpc": "2.0", "id": 1, "error": { "code": -32601, "message": "Method not found" } },
                { "jsonrpc": "2.0", "id": 2, "error": { "code": -32601, "message": "Method not found" } }
            ]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "assets": [] }));
    });

    let out = rwa(&home)
        .args(["--json", "gm", "portfolio", WALLET])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .output()
        .unwrap();

    assert!(!out.status.success(), "RPC failure must exit non-zero");
    let v = stdout_json(&out);
    assert_eq!(v["status"], "error");
    assert_eq!(v["error_kind"], "rpc_unavailable");
    assert!(v["error"].as_str().unwrap().contains("Method not found"));
}

/// `gm buy --quote-only --json` emits the `dry_run` TradeJson shape without
/// touching the RPC (no funds check). Sessions are wall-clock dependent, so
/// when the market is closed the same invocation must emit the error envelope
/// with `error_kind: "market_closed"` — both branches are stable contracts.
#[test]
fn buy_quote_only_json_emits_dry_run_shape() {
    let home = test_home("buy-quote-only");
    let server = MockServer::start();

    // Quote-only must never need funds — a dead RPC proves it stays off-chain.
    server.mock(|when, then| {
        when.method(POST).path("/rpc");
        then.status(500);
    });
    // Session-limits API down → check_tradable fails open (documented behavior).
    server.mock(|when, then| {
        when.method(GET).path("/session");
        then.status(500);
    });
    let order = server.mock(|when, then| {
        when.method(GET).path("/order");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "requestId": "req-1",
                "inAmount": "10000000",
                "outAmount": "1000000000",
                "inUsdValue": 10.0,
                "outUsdValue": 9.99,
                "feeBps": 5,
                "gasless": true,
                "router": "iris",
                "transaction": "AQABBASE64DUMMYTX=="
            }));
    });

    let keygen = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(keygen.status.success(), "keys generate failed: {}", String::from_utf8_lossy(&keygen.stderr));

    let out = rwa(&home)
        .args(["--json", "gm", "buy", "AAL", "10", "--quote-only"])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_SESSION_URL", server.url("/session"))
        .env("RWA_JUPITER_URL", server.base_url())
        .output()
        .unwrap();

    let v = stdout_json(&out);
    match v["status"].as_str() {
        Some("dry_run") => {
            assert_eq!(v["token"], "AALon");
            assert_eq!(v["counter_amount"], "10");
            assert_eq!(v["counter_token"], "USDC");
            assert_eq!(v["tx"], "");
            assert_eq!(v["fee_bps"], 5);
            assert_eq!(v["gasless"], true);
            assert_eq!(v["router"], "iris");
            order.assert_hits(1);
        }
        Some("error") => {
            // Market closed right now (weekend window) — still a stable contract.
            assert_eq!(v["error_kind"], "market_closed", "unexpected error: {v}");
            assert!(!out.status.success());
        }
        other => panic!("unexpected status {other:?}: {v}"),
    }
}
