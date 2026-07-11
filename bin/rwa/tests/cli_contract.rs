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
        "RWA_WALLET",
        "RWA_ONDO_API_URL",
        "RWA_ONDO_SESSION_URL",
        "RWA_JUPITER_URL",
        "RWA_NO_AUTO_GAS",
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

/// `buy-basket --dry-run --max-bps N` rejects quotes whose all-in cost exceeds
/// the ceiling: items land in `failed[]` with `error_kind: "cost_too_high"`.
/// Market-closed windows emit the `market_closed` envelope instead — both are
/// stable contracts.
#[test]
fn buy_basket_dry_run_enforces_max_bps() {
    let home = test_home("basket-max-bps");
    let server = MockServer::start();

    // check_tradable's trading-paused gate: empty list keeps every symbol
    // (including live-paused ones) resolved as "not found" == not paused.
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "assets": [] }));
    });
    // preflight_basket_buy: SOL via getBalance, USDC via getTokenAccountsByOwner.
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getBalance");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!(
                { "jsonrpc": "2.0", "id": 1, "result": { "context": { "slot": 1 }, "value": 1_000_000_000u64 } }
            ));
    });
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getTokenAccountsByOwner");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "context": { "slot": 1 }, "value": [{
                    "pubkey": "SomePubkeyAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    "account": {
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                                    "owner": WALLET,
                                    "tokenAmount": {
                                        "amount": "1000000000",
                                        "decimals": 6,
                                        "uiAmount": 1000.0,
                                        "uiAmountString": "1000"
                                    }
                                },
                                "type": "account"
                            },
                            "program": "spl-token",
                            "space": 165
                        },
                        "executable": false,
                        "lamports": 2039280,
                        "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                        "rentEpoch": 0
                    }
                }] }
            }));
    });
    // Deterministic session fixture (AAL tradable in EVERY session) instead of a
    // 500: a 500 relies on the fail-open path, which only applies OUTSIDE the
    // Closed session — so when this test ran during a real "Closed" wall-clock it
    // failed closed with `market_closed` instead of reaching the cost gate. The
    // fixture makes the cost_too_high assertion wall-clock-independent.
    server.mock(|when, then| {
        when.method(GET).path("/session");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(always_tradable_limits());
    });
    // Quote: spread −0.5% (50 bps) + fee 50 bps = 100 bps all-in, over the 10 bps cap.
    server.mock(|when, then| {
        when.method(GET).path("/order");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "requestId": "req-1",
                "inAmount": "10000000",
                "outAmount": "1000000000",
                "inUsdValue": 10.0,
                "outUsdValue": 9.95,
                "feeBps": 50,
                "router": "iris",
                "transaction": "AQABBASE64DUMMYTX=="
            }));
    });

    let keygen = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(keygen.status.success(), "keys generate failed: {}", String::from_utf8_lossy(&keygen.stderr));

    let out = rwa(&home)
        .args(["--json", "gm", "buy-basket", "AAL", "10", "--dry-run", "--max-bps", "10"])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .env("RWA_ONDO_SESSION_URL", server.url("/session"))
        .env("RWA_JUPITER_URL", server.base_url())
        .output()
        .unwrap();

    let v = stdout_json(&out);
    match v["status"].as_str() {
        Some("dry_run") => {
            assert!(v["bought"].as_array().unwrap().is_empty(), "over-budget quote must not pass: {v}");
            let failed = v["failed"].as_array().unwrap();
            assert_eq!(failed.len(), 1, "expected one cost_too_high failure: {v}");
            // failed[].token echoes the user's input symbol (existing fail_json convention)
            assert_eq!(failed[0]["token"], "AAL");
            assert_eq!(failed[0]["error_kind"], "cost_too_high");
        }
        Some("error") => {
            assert_eq!(v["error_kind"], "market_closed", "unexpected error: {v}");
        }
        other => panic!("unexpected status {other:?}: {v}"),
    }
}

/// `gm buy --quote-only --json` emits the `dry_run` TradeJson shape without
/// touching the RPC (no funds check). Sessions are wall-clock dependent, so
/// when the market is closed the same invocation must emit the error envelope
/// with `error_kind: "market_closed"` — both branches are stable contracts.
#[test]
fn buy_quote_only_json_emits_dry_run_shape() {
    let home = test_home("buy-quote-only");
    let server = MockServer::start();

    // check_tradable's trading-paused gate: empty list keeps every symbol
    // (including live-paused ones) resolved as "not found" == not paused.
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "assets": [] }));
    });
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
        .env("RWA_ONDO_API_URL", server.url("/assets"))
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

/// `gm buy --json` without `-y` (and not `--dry-run`/`--quote-only`) must fail
/// closed with `confirmation_required` before ever reaching `/execute` — the
/// v0.6.0 breaking change: `--json` alone no longer silently executes real
/// trades (see CLAUDE.md "Agent usage rules").
#[test]
fn buy_json_without_yes_is_confirmation_required_and_never_executes() {
    let home = test_home("buy-confirmation-required");
    let server = MockServer::start();

    // check_tradable's trading-paused gate: empty list keeps every symbol
    // (including live-paused ones, e.g. TSLA) resolved as "not found" == not
    // paused. Without this the preflight hits the live Ondo assets API.
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "assets": [] }));
    });
    // auto_gas + preflight: healthy SOL/USDC balances, no refuel needed.
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getBalance");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!(
                { "jsonrpc": "2.0", "id": 1, "result": { "context": { "slot": 1 }, "value": 1_000_000_000u64 } }
            ));
    });
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getTokenAccountsByOwner");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "context": { "slot": 1 }, "value": [{
                    "pubkey": "SomePubkeyAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    "account": {
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                                    "owner": WALLET,
                                    "tokenAmount": {
                                        "amount": "1000000000",
                                        "decimals": 6,
                                        "uiAmount": 1000.0,
                                        "uiAmountString": "1000"
                                    }
                                },
                                "type": "account"
                            },
                            "program": "spl-token",
                            "space": 165
                        },
                        "executable": false,
                        "lamports": 2039280,
                        "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                        "rentEpoch": 0
                    }
                }] }
            }));
    });
    // get_mint_multiplier: best-effort, ignored without --limit-price — "not
    // found" is a fine answer.
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getAccountInfo");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": { "value": null } }));
    });
    // All-sessions-tradable fixture keeps this wall-clock independent
    // (including weekend offhours).
    server.mock(|when, then| {
        when.method(GET).path("/session");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(always_tradable_limits());
    });
    let order = server.mock(|when, then| {
        when.method(GET).path("/order");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "requestId": "req-1",
                "inAmount": "5000000",
                "outAmount": "10000000",
                "inUsdValue": 5.0,
                "outUsdValue": 4.99,
                "feeBps": 5,
                "gasless": true,
                "router": "iris",
                "transaction": "AQABBASE64DUMMYTX=="
            }));
    });
    let execute = server.mock(|when, then| {
        when.method(POST).path("/execute");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "status": "Success", "signature": "sig" }));
    });

    let keygen = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(keygen.status.success(), "keys generate failed: {}", String::from_utf8_lossy(&keygen.stderr));

    let out = rwa(&home)
        .args(["--json", "gm", "buy", "TSLA", "5"])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .env("RWA_ONDO_SESSION_URL", server.url("/session"))
        .env("RWA_JUPITER_URL", server.base_url())
        .output()
        .unwrap();

    assert!(!out.status.success(), "json without -y must not execute");
    // confirmation_required is not a transient kind — exit 1, not 75.
    assert_eq!(out.status.code(), Some(1));
    let v = stdout_json(&out);
    assert_eq!(v["status"], "error");
    assert_eq!(v["error_kind"], "confirmation_required");
    let msg = v["error"].as_str().unwrap();
    assert!(msg.contains("-y"), "teaches the fix: {msg}");
    assert!(msg.contains("--dry-run"), "offers the preview path: {msg}");

    // The order was quoted (preview is harmless) but /execute must never be hit.
    order.assert_hits(1);
    execute.assert_hits(0);
}

// ── Named wallets ────────────────────────────────────────────────────────────

/// Extract the `Key file: <path>` line printed by `keys generate`.
fn parse_key_file_path(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("Key file:"))
        .map(|s| s.trim().to_string())
        .expect("generate prints a 'Key file:' line")
}

#[test]
fn named_wallets_add_list_use_remove() {
    let home = test_home("named-wallets");

    // 1) Create a plaintext default wallet (no passphrase needed).
    let gen_out = rwa(&home)
        .args(["keys", "generate", "--allow-plaintext"])
        .output()
        .unwrap();
    assert!(gen_out.status.success(), "generate failed: {}", String::from_utf8_lossy(&gen_out.stderr));
    let key_path = parse_key_file_path(&String::from_utf8_lossy(&gen_out.stdout));

    // 2) list --json lazily registers the legacy key as `default` (active).
    let list1 = rwa(&home).args(["keys", "list", "--json"]).output().unwrap();
    assert!(list1.status.success());
    let v1 = stdout_json(&list1);
    let wallets = v1.get("wallets").and_then(|w| w.as_array()).unwrap();
    let def = wallets.iter().find(|w| w["name"] == "default").expect("default present");
    assert_eq!(def["active"], serde_json::Value::Bool(true));
    assert_eq!(def["encrypted"], serde_json::Value::Bool(false));
    assert!(def["pubkey"].is_string(), "plaintext wallet exposes pubkey");

    // 3) Register the same file under a second name.
    let add = rwa(&home)
        .args(["keys", "add", "backup", "--path", &key_path])
        .output()
        .unwrap();
    assert!(add.status.success(), "add failed: {}", String::from_utf8_lossy(&add.stderr));

    // 4) Switch active to backup.
    let use_ = rwa(&home).args(["keys", "use", "backup"]).output().unwrap();
    assert!(use_.status.success());
    let v2 = stdout_json(&rwa(&home).args(["keys", "list", "--json"]).output().unwrap());
    let w2 = v2["wallets"].as_array().unwrap();
    let backup = w2.iter().find(|w| w["name"] == "backup").unwrap();
    assert_eq!(backup["active"], serde_json::Value::Bool(true));
    let default2 = w2.iter().find(|w| w["name"] == "default").unwrap();
    assert_eq!(default2["active"], serde_json::Value::Bool(false), "default must be inactive after `use backup`");

    // 5) Remove backup (it was active) — registry stays valid, active cleared.
    let rm = rwa(&home).args(["keys", "remove", "backup"]).output().unwrap();
    assert!(rm.status.success());
    let v3 = stdout_json(&rwa(&home).args(["keys", "list", "--json"]).output().unwrap());
    let remaining = v3["wallets"].as_array().unwrap();
    let names: Vec<_> = remaining.iter().map(|w| w["name"].clone()).collect();
    assert!(!names.contains(&serde_json::Value::from("backup")), "backup removed");
    assert!(remaining.iter().any(|w| w["name"] == "default"), "default still present after removing backup");
    assert!(!remaining.iter().any(|w| w["active"] == serde_json::Value::Bool(true)), "active pointer cleared after removing the active wallet");
}

#[test]
fn unknown_selected_wallet_errors() {
    let home = test_home("unknown-wallet");
    let out = rwa(&home)
        .args(["--json", "--wallet", "ghost", "keys", "show"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "unknown wallet must fail");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("ghost"), "error names the bad wallet: {combined}");
    assert!(combined.contains("not found"), "error says not found: {combined}");
}

// ── keys add: import from seed phrase / private key ──────────────────────────

/// Standard BIP39 test mnemonic (also used in crates/ondo wallet unit tests).
const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[test]
fn keys_add_import_from_seed_plaintext_is_deterministic() {
    let home = test_home("add-seed-plain");
    let p1 = home.join("k1.json");
    let p2 = home.join("k2.json");

    // Import the same seed twice, under two names, as plaintext (no passphrase).
    for (name, path) in [("s1", &p1), ("s2", &p2)] {
        let out = rwa(&home)
            .args(["keys", "add", name, "--seed-phrase", TEST_MNEMONIC, "--allow-plaintext", "--path"])
            .arg(path)
            .output()
            .unwrap();
        assert!(out.status.success(), "add {name} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    // The written file is a 64-byte solana-keygen JSON array.
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(&p1).unwrap()).unwrap();
    assert_eq!(bytes.len(), 64, "plaintext key file must be a 64-byte array");

    // Both registered wallets are plaintext with the SAME derived pubkey (determinism).
    let v = stdout_json(&rwa(&home).args(["keys", "list", "--json"]).output().unwrap());
    let w = v["wallets"].as_array().unwrap();
    let s1 = w.iter().find(|x| x["name"] == "s1").unwrap();
    let s2 = w.iter().find(|x| x["name"] == "s2").unwrap();
    assert_eq!(s1["encrypted"], serde_json::Value::Bool(false));
    assert!(s1["pubkey"].is_string(), "plaintext import exposes a pubkey");
    assert_eq!(s1["pubkey"], s2["pubkey"], "same seed must derive the same address");
}

#[test]
fn keys_add_import_encrypted_by_default() {
    let home = test_home("add-seed-enc");
    let pass = "TestPass2026!secure";
    let cold = home.join("cold.age");
    let warm = home.join("warm.json");

    // Encrypted by default (passphrase from RWA_PASSPHRASE).
    let enc = rwa(&home)
        .env("RWA_PASSPHRASE", pass)
        .args(["keys", "add", "cold", "--seed-phrase", TEST_MNEMONIC, "--path"])
        .arg(&cold)
        .output()
        .unwrap();
    assert!(enc.status.success(), "encrypted add failed: {}", String::from_utf8_lossy(&enc.stderr));

    // The file is genuinely age-encrypted.
    let head = std::fs::read(&cold).unwrap();
    assert!(head.starts_with(b"age-encryption.org/v1"), "default add must write an age file");

    // list --json: cold is encrypted with pubkey null (no passphrase prompt for listing).
    let v = stdout_json(&rwa(&home).args(["keys", "list", "--json"]).output().unwrap());
    let cold_e = v["wallets"].as_array().unwrap().iter().find(|x| x["name"] == "cold").unwrap();
    assert_eq!(cold_e["encrypted"], serde_json::Value::Bool(true));
    assert_eq!(cold_e["pubkey"], serde_json::Value::Null);

    // A plaintext import of the same seed exposes the address; the encrypted wallet,
    // shown with its passphrase, must resolve to the same address (roundtrip).
    let warm_out = rwa(&home)
        .args(["keys", "add", "warm", "--seed-phrase", TEST_MNEMONIC, "--allow-plaintext", "--path"])
        .arg(&warm)
        .output()
        .unwrap();
    assert!(warm_out.status.success(), "warm add failed: {}", String::from_utf8_lossy(&warm_out.stderr));
    let v2 = stdout_json(&rwa(&home).args(["keys", "list", "--json"]).output().unwrap());
    let warm_pk = v2["wallets"].as_array().unwrap().iter()
        .find(|x| x["name"] == "warm").unwrap()["pubkey"].as_str().unwrap().to_string();

    let show = rwa(&home)
        .env("RWA_PASSPHRASE", pass)
        .args(["--wallet", "cold", "keys", "show"])
        .output()
        .unwrap();
    assert!(show.status.success(), "show cold failed: {}", String::from_utf8_lossy(&show.stderr));
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(show_out.contains(&warm_pk), "encrypted wallet must resolve to the same address as the plaintext one:\n{show_out}");
}

/// `keys encrypt --json` and `keys decrypt --json` must emit JSON on SUCCESS,
/// not human prose (both advertise --json in --help). The bug was invisible
/// because the ERROR paths already emit JSON (via render_error) and no test
/// touched the success path. Also proves `keys decrypt` honours RWA_PASSPHRASE
/// (it read a TTY-only prompt before, so it couldn't be scripted at all).
#[test]
fn keys_encrypt_decrypt_json_emit_json_on_success() {
    let home = test_home("encrypt-json");
    let pass = "TestPass2026!secure";

    // Start from a plaintext wallet.
    let g = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(g.status.success(), "generate failed: {}", String::from_utf8_lossy(&g.stderr));

    // encrypt --json → JSON, not "Wallet encrypted." prose.
    let enc = rwa(&home)
        .env("RWA_PASSPHRASE", pass)
        .args(["--json", "keys", "encrypt"])
        .output()
        .unwrap();
    assert!(enc.status.success(), "encrypt failed: {}", String::from_utf8_lossy(&enc.stderr));
    let v = stdout_json(&enc);
    assert_eq!(v["status"], "ok", "encrypt --json must be JSON, got: {}", String::from_utf8_lossy(&enc.stdout));
    assert_eq!(v["encrypted"], serde_json::Value::Bool(true));

    // decrypt --json → JSON, AND reads RWA_PASSPHRASE (no TTY available here).
    let dec = rwa(&home)
        .env("RWA_PASSPHRASE", pass)
        .args(["--json", "keys", "decrypt"])
        .output()
        .unwrap();
    assert!(dec.status.success(), "decrypt (scripted via RWA_PASSPHRASE) failed: {}", String::from_utf8_lossy(&dec.stderr));
    let v = stdout_json(&dec);
    assert_eq!(v["status"], "ok", "decrypt --json must be JSON, got: {}", String::from_utf8_lossy(&dec.stdout));
    assert_eq!(v["encrypted"], serde_json::Value::Bool(false));
}

#[test]
fn keys_add_import_refuses_to_overwrite_existing_file() {
    let home = test_home("add-no-overwrite");
    let p = home.join("taken.json");
    std::fs::write(&p, b"do not clobber").unwrap();

    let out = rwa(&home)
        .args(["keys", "add", "x", "--seed-phrase", TEST_MNEMONIC, "--allow-plaintext", "--path"])
        .arg(&p)
        .output()
        .unwrap();
    assert!(!out.status.success(), "importing onto an existing file must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("overwrite") || err.contains("existing"), "error explains the refusal: {err}");
    // The original file is untouched.
    assert_eq!(std::fs::read(&p).unwrap(), b"do not clobber");
}

// ── hours / history ──────────────────────────────────────────────────────────

/// Session-limits fixture where both tokens are tradable in EVERY session
/// (including weekend `offhours`), so assertions hold at any wall-clock time.
fn always_tradable_limits() -> serde_json::Value {
    let all = serde_json::json!({
        "tradable": true, "maxAttestationCount": "500", "maxActiveNotionalValue": "200000"
    });
    serde_json::json!({
        "limits": [
            { "symbol": "AALon",  "premarket": all, "regular": all, "postmarket": all,
              "overnight": all, "offhours": all },
            { "symbol": "TSLAon", "premarket": all, "regular": all, "postmarket": all,
              "overnight": all, "offhours": all }
        ]
    })
}

/// `gm hours --tradable --json` emits the stable HoursJson shape; with an
/// all-sessions-tradable fixture the tradable set is wall-clock independent —
/// including weekends, where the offhours session now applies.
#[test]
fn hours_json_emits_shape_and_tradable_set() {
    let home = test_home("hours-shape");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/session");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(always_tradable_limits());
    });
    // Fixture with 3+ assets: AALon (normal), TSLAon (isTradingPaused), SPYon (isOffhoursTradable)
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "assets": [
                    {
                        "symbol": "AALon",
                        "assetName": "American Airlines",
                        "isTradingPaused": false,
                        "isOffhoursTradable": false,
                        "primaryMarket": { "price": "10.5" }
                    },
                    {
                        "symbol": "TSLAon",
                        "assetName": "Tesla",
                        "isTradingPaused": true,
                        "isOffhoursTradable": false,
                        "primaryMarket": { "price": "250.0" }
                    },
                    {
                        "symbol": "SPYon",
                        "assetName": "SPY ETF",
                        "isTradingPaused": false,
                        "isOffhoursTradable": true,
                        "primaryMarket": { "price": "450.0" }
                    }
                ]
            }));
    });

    let out = rwa(&home)
        .args(["--json", "gm", "hours", "--tradable"])
        .env("RWA_ONDO_SESSION_URL", server.url("/session"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let v = stdout_json(&out);
    assert!(matches!(v["status"].as_str(), Some("open" | "closed")));
    assert!(v["session"].is_string() && v["session_hours"].is_string());
    assert!(v["now"].as_str().unwrap().contains("ET"));
    assert!(v["countdown"].is_string());
    assert_eq!(v["tradable_count"], 2);
    let tradable: Vec<&str> = v["tradable"].as_array().unwrap()
        .iter().map(|s| s.as_str().unwrap()).collect();
    assert_eq!(tradable, vec!["AALon", "TSLAon"]);
    // Assets contract: paused and offhours_tradable counts and array with --tradable.
    assert_eq!(v["paused_count"], 1, "exactly one asset has isTradingPaused=true");
    assert_eq!(v["offhours_tradable_count"], 1, "exactly one asset has isOffhoursTradable=true");
    let offhours: Vec<&str> = v["offhours_tradable"].as_array().unwrap()
        .iter().map(|s| s.as_str().unwrap()).collect();
    assert_eq!(offhours, vec!["SPYon"], "offhours_tradable array includes flagged symbols");
}

/// HUMAN-mode guard: `gm hours --tradable` (no `--json`) must actually LIST the
/// flagship symbols, not just their count — this class of human-output drift
/// (the flag was a no-op in human mode, contradicting the docs) previously had
/// no test, since the contract suite only locked the JSON shapes.
#[test]
fn hours_tradable_human_lists_flagship_symbols() {
    let home = test_home("hours-human");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/session");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(always_tradable_limits());
    });
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "assets": [
                    { "symbol": "SPYon", "assetName": "SPY ETF", "isTradingPaused": false,
                      "isOffhoursTradable": true, "primaryMarket": { "price": "450.0" } }
                ]
            }));
    });

    let out = rwa(&home)
        .args(["gm", "hours", "--tradable"]) // NO --json → human output
        .env("RWA_ONDO_SESSION_URL", server.url("/session"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("SPYon"),
        "--tradable must LIST the flagship symbol in human mode, got:\n{text}"
    );
    assert!(text.contains("flagship"), "human output should label the flagship set: {text}");
}

/// `gm history --json` emits the HistoryJson shape with hand-derived
/// aggregates from the mocked candles.
#[test]
fn history_json_emits_shape_with_derived_aggregates() {
    let home = test_home("history-shape");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path("/tslaon/history")
            .query_param("range", "1month");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "primaryMarketPrice": [
                    { "timestamp": 1000, "value": 100.0, "open": 100.0, "high": 111.0, "low": 99.0, "close": 105.0 },
                    { "timestamp": 2000, "value": 110.0, "open": 105.0, "high": 105.0, "low": 95.0,  "close": 110.0 }
                ]
            }));
    });

    let out = rwa(&home)
        .args(["--json", "gm", "history", "TSLA"])
        .env("RWA_ONDO_API_URL", server.base_url())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let v = stdout_json(&out);
    assert_eq!(v["symbol"], "TSLAon");
    assert_eq!(v["range"], "1M");
    assert_eq!(v["candles"], 2);
    assert_eq!(v["first"]["timestamp"], 1000);
    assert_eq!(v["first"]["price"], 100.0, "first.price is the opening price");
    assert_eq!(v["last"]["timestamp"], 2000);
    assert_eq!(v["last"]["price"], 110.0, "last.price is the closing price");
    assert_eq!(v["high"], 111.0);
    assert_eq!(v["low"], 95.0);
    // (110 - 100) / 100 × 100 = +10%
    assert_eq!(v["change_pct"], 10.0);
}

// ── send / reclaim / close-all ───────────────────────────────────────────────

/// One USDC token-account entry in `getTokenAccountsByOwner` shape.
fn usdc_account_entry(raw_amount: u64) -> serde_json::Value {
    serde_json::json!({
        "pubkey": "SomeUsdcAtaAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "account": {
            "data": {
                "parsed": {
                    "info": {
                        "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                        "owner": WALLET,
                        "tokenAmount": {
                            "amount": raw_amount.to_string(),
                            "decimals": 6,
                            "uiAmount": raw_amount as f64 / 1_000_000.0,
                            "uiAmountString": (raw_amount as f64 / 1_000_000.0).to_string()
                        }
                    },
                    "type": "account"
                },
                "program": "spl-token",
                "space": 165
            },
            "executable": false,
            "lamports": 2039280,
            "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
            "rentEpoch": 0
        }
    })
}

/// Mock the RPC methods the send/reclaim paths touch, dispatched by method
/// name in the request body.
fn mock_rpc_for_transfers(server: &MockServer, usdc_raw: u64, token_accounts: serde_json::Value) {
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getBalance");
        then.status(200).json_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "context": { "slot": 1 }, "value": 76_600_000u64 } // 0.0766 SOL
        }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getTokenAccountsByOwner");
        then.status(200).json_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "context": { "slot": 1 }, "value":
                if usdc_raw > 0 { serde_json::json!([usdc_account_entry(usdc_raw)]) } else { token_accounts.clone() } }
        }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getRecentPrioritizationFees");
        then.status(200).json_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": []
        }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getMinimumBalanceForRentExemption");
        then.status(200).json_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": 2_039_280u64
        }));
    });
}

/// `gm send USDC <amt> <to> --dry-run --json` emits the SendJson dry_run shape
/// and never submits a transaction (no sendTransaction call possible — the
/// mock has no handler for it, so submission would fail loudly).
#[test]
fn send_usdc_dry_run_emits_send_json_shape() {
    let home = test_home("send-dry-run");
    let server = MockServer::start();
    mock_rpc_for_transfers(&server, 82_310_000, serde_json::json!([]));

    let keygen = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(keygen.status.success());

    let recipient = "Dn9EqxugBePrno7gzCjbGeYxY3VJE9RB2WE2FH7t7qmH";
    let out = rwa(&home)
        .args(["--json", "gm", "send", "USDC", "0.1", recipient, "--dry-run"])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let v = stdout_json(&out);
    assert_eq!(v["status"], "dry_run");
    assert_eq!(v["token"], "USDC");
    assert_eq!(v["amount"], "0.1");
    assert_eq!(v["recipient"], recipient);
    assert_eq!(v["tx"], "", "dry run must not carry a tx link");
}

/// `gm reclaim --json` with no empty token accounts: success, zero closed,
/// zero reclaimed — and exit 0.
#[test]
fn reclaim_json_reports_nothing_to_reclaim() {
    let home = test_home("reclaim-empty");
    let server = MockServer::start();
    mock_rpc_for_transfers(&server, 0, serde_json::json!([]));

    let keygen = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(keygen.status.success());

    let out = rwa(&home)
        .args(["--json", "gm", "reclaim"])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let v = stdout_json(&out);
    assert_eq!(v["status"], "success");
    assert_eq!(v["accounts_closed"], 0);
    assert_eq!(v["sol_reclaimed"], "0");
    assert_eq!(v["signatures"].as_array().unwrap().len(), 0);
}

/// `gm close-all --dry-run --json` with no GM positions: success with empty
/// arrays and total "0".
#[test]
fn close_all_dry_run_reports_no_positions() {
    let home = test_home("close-all-empty");
    let server = MockServer::start();
    mock_rpc_for_transfers(&server, 0, serde_json::json!([]));
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200).json_body(serde_json::json!({ "assets": [] }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/session");
        then.status(200).json_body(always_tradable_limits());
    });

    let keygen = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(keygen.status.success());

    let out = rwa(&home)
        .args(["--json", "gm", "close-all", "--dry-run"])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .env("RWA_ONDO_SESSION_URL", server.url("/session"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let v = stdout_json(&out);
    assert_eq!(v["status"], "success");
    assert_eq!(v["sold"].as_array().unwrap().len(), 0);
    assert_eq!(v["failed"].as_array().unwrap().len(), 0);
    assert!(v.get("skipped").is_none(), "empty skipped must be omitted");
    assert_eq!(v["total_usdc"], "0");
}

/// `gm sell <SYM> all --dry-run --json` walks the full prepare path — RPC
/// balance, session-limits tradability (all-sessions fixture, wall-clock
/// independent), Jupiter quote — and emits the dry_run TradeJson shape
/// without executing.
#[test]
fn sell_all_dry_run_emits_trade_json_shape() {
    let home = test_home("sell-dry-run");
    let server = MockServer::start();

    // check_tradable's trading-paused gate: empty list keeps every symbol
    // (including live-paused ones) resolved as "not found" == not paused.
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "assets": [] }));
    });
    // AAL position: 2.0 tokens (9 decimals) on the wallet.
    server.mock(|when, then| {
        when.method(POST).path("/rpc").body_contains("getTokenAccountsByOwner");
        then.status(200).json_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "context": { "slot": 1 },
                        "value": [token_entry(AAL_MINT, 2.0, "2000000000")] }
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/session");
        then.status(200).json_body(always_tradable_limits());
    });
    let order = server.mock(|when, then| {
        when.method(GET)
            .path("/order")
            // Selling ALL must quote the full raw balance of the GM mint.
            .query_param("inputMint", AAL_MINT)
            .query_param("amount", "2000000000");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "requestId": "req-sell-1",
                "inAmount": "2000000000",
                "outAmount": "25000000",
                "inUsdValue": 25.0,
                "outUsdValue": 24.97,
                "feeBps": 10,
                "gasless": false,
                "router": "jupiterz",
                "transaction": "AQABBASE64DUMMYTX=="
            }));
    });

    let keygen = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(keygen.status.success());

    let out = rwa(&home)
        .args(["--json", "gm", "sell", "AAL", "all", "--dry-run"])
        .env("RWA_RPC_URL", server.url("/rpc"))
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .env("RWA_ONDO_SESSION_URL", server.url("/session"))
        .env("RWA_JUPITER_URL", server.base_url())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let v = stdout_json(&out);
    assert_eq!(v["status"], "dry_run");
    assert_eq!(v["token"], "AALon");
    assert_eq!(v["amount"], "2", "sell all = the whole 2.0 position");
    assert_eq!(v["counter_token"], "USDC");
    assert_eq!(v["counter_amount"], "25", "quoted outAmount formatted to USDC");
    assert_eq!(v["tx"], "", "dry run must not carry a tx link");
    assert_eq!(v["fee_bps"], 10);
    assert_eq!(v["router"], "jupiterz");
    order.assert_hits(1);
}

// ── keys export round trip ───────────────────────────────────────────────────

/// The full key lifecycle contract: generate (mnemonic-first) → export
/// (--reveal) → re-import both secrets into fresh homes → identical address.
/// Also pins the export JSON shape and the no-reveal refusal.
#[test]
fn keys_export_roundtrip_restores_the_same_wallet() {
    let home = test_home("keys-export");

    let generated = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(generated.status.success(), "generate: {}", String::from_utf8_lossy(&generated.stderr));
    let gen_stdout = String::from_utf8_lossy(&generated.stdout).to_string();
    assert!(
        gen_stdout.contains("Recovery phrase"),
        "generate must print the phrase once: {gen_stdout}"
    );
    let phrase_line = gen_stdout
        .lines()
        .skip_while(|l| !l.contains("Recovery phrase"))
        .nth(1)
        .expect("phrase follows the header")
        .trim()
        .to_string();
    assert_eq!(phrase_line.split_whitespace().count(), 12, "12-word phrase");

    // --json without --reveal must refuse.
    let refused = rwa(&home).args(["--json", "keys", "export"]).output().unwrap();
    assert!(!refused.status.success(), "export without --reveal must fail");

    let exp = rwa(&home).args(["--json", "keys", "export", "--reveal"]).output().unwrap();
    assert!(exp.status.success(), "export: {}", String::from_utf8_lossy(&exp.stderr));
    let v = stdout_json(&exp);
    let pubkey = v["pubkey"].as_str().unwrap().to_string();
    let b58 = v["private_key_base58"].as_str().unwrap().to_string();
    assert_eq!(v["private_key_json"].as_array().unwrap().len(), 64);
    // Plaintext wallet: phrase is not stored.
    assert!(v["mnemonic"].is_null());

    // Re-import the exported base58 key in a fresh home → same address.
    let home2 = test_home("keys-export-reimport-b58");
    let imp = rwa(&home2)
        .args(["keys", "import", "--private-key", &b58, "--allow-plaintext"])
        .output()
        .unwrap();
    assert!(imp.status.success(), "import: {}", String::from_utf8_lossy(&imp.stderr));
    assert!(
        String::from_utf8_lossy(&imp.stdout).contains(&pubkey),
        "base58 re-import must restore the same address"
    );

    // Re-import the printed mnemonic in another fresh home → same address.
    let home3 = test_home("keys-export-reimport-phrase");
    let imp2 = rwa(&home3)
        .args(["keys", "import", "--seed-phrase", &phrase_line, "--allow-plaintext"])
        .output()
        .unwrap();
    assert!(imp2.status.success(), "import phrase: {}", String::from_utf8_lossy(&imp2.stderr));
    assert!(
        String::from_utf8_lossy(&imp2.stdout).contains(&pubkey),
        "mnemonic re-import must restore the same address"
    );
}

/// Encrypted wallets store the phrase inside the age payload: export reveals
/// it (with RWA_PASSPHRASE) and it restores the same wallet.
#[test]
fn keys_export_reveals_stored_mnemonic_for_encrypted_wallet() {
    let home = test_home("keys-export-enc");
    let pass = "TestPass2026!secure";

    let generated = rwa(&home)
        .args(["keys", "generate"])
        .env("RWA_PASSPHRASE", pass)
        .output()
        .unwrap();
    // `keys generate` prompts interactively for a NEW passphrase (env is for
    // unlocking, not creation) — if it can't prompt it fails; fall back to
    // seed-phrase import, which exercises the same storage path.
    let (home, pubkey) = if generated.status.success() {
        let out = String::from_utf8_lossy(&generated.stdout);
        let pk = out.lines().find_map(|l| l.strip_prefix("Address:")).unwrap().trim().to_string();
        (home, pk)
    } else {
        let home2 = test_home("keys-export-enc2");
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let imp = rwa(&home2)
            .args(["keys", "add", "main", "--seed-phrase", phrase, "--path"])
            .arg(home2.join("main.age"))
            .env("RWA_PASSPHRASE", pass)
            .output()
            .unwrap();
        assert!(imp.status.success(), "add: {}", String::from_utf8_lossy(&imp.stderr));
        (home2, "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk".to_string())
    };

    let exp = rwa(&home)
        .args(["--json", "keys", "export", "--reveal"])
        .env("RWA_PASSPHRASE", pass)
        .output()
        .unwrap();
    assert!(exp.status.success(), "export: {}", String::from_utf8_lossy(&exp.stderr));
    let v = stdout_json(&exp);
    assert_eq!(v["pubkey"].as_str().unwrap(), pubkey);
    let mnemonic = v["mnemonic"].as_str().expect("encrypted wallet stores the phrase");
    assert!(mnemonic.split_whitespace().count() >= 12);
}

// ── pnl ──────────────────────────────────────────────────────────────────────

/// `gm pnl --json` computes average cost, realized and unrealized P&L from a
/// pre-seeded trade ledger — all expected numbers hand-derived.
#[test]
fn pnl_json_computes_from_the_ledger() {
    let home = test_home("pnl-shape");
    let server = MockServer::start();
    // AALon market price: 12.00 (for the unrealized leg).
    server.mock(|when, then| {
        when.method(GET).path("/assets");
        then.status(200).json_body(serde_json::json!({
            "assets": [{
                "symbol": "AALon", "assetName": "American Airlines",
                "primaryMarket": { "price": "12", "priceChangePct24h": "0" }
            }]
        }));
    });

    let generated = rwa(&home).args(["keys", "generate", "--allow-plaintext"]).output().unwrap();
    assert!(generated.status.success());
    let list = rwa(&home).args(["keys", "list", "--json"]).output().unwrap();
    let lv = stdout_json(&list);
    let pubkey = lv["wallets"][0]["pubkey"].as_str().unwrap().to_string();
    // Locate the binary's real config dir portably (macOS uses
    // Library/Application Support, not XDG): the key file lives in it.
    let key_path = PathBuf::from(lv["wallets"][0]["path"].as_str().unwrap());
    let rwa_config_dir = key_path.parent().unwrap().to_path_buf();

    // Seed the ledger: buy 1.0 @ 10, buy 1.0 @ 14 → avg 12;
    // then sell 1.0 for 15 → realized 15 − 12 = +3; left: 1.0 invested 12.
    // Market 12.00 → unrealized 12 − 12 = 0.
    let ledger_dir = rwa_config_dir.join("ledger");
    std::fs::create_dir_all(&ledger_dir).unwrap();
    std::fs::write(
        ledger_dir.join(format!("{pubkey}.jsonl")),
        concat!(
            "{\"ts\":\"2026-07-01T00:00:00Z\",\"sig\":\"s1\",\"kind\":\"buy\",\"token\":\"AALon\",\"qty_raw\":\"1000000000\",\"usdc_raw\":\"10000000\"}\n",
            "{\"ts\":\"2026-07-01T01:00:00Z\",\"sig\":\"s2\",\"kind\":\"buy\",\"token\":\"AALon\",\"qty_raw\":\"1000000000\",\"usdc_raw\":\"14000000\"}\n",
            "{\"ts\":\"2026-07-01T02:00:00Z\",\"sig\":\"s3\",\"kind\":\"sell\",\"token\":\"AALon\",\"qty_raw\":\"1000000000\",\"usdc_raw\":\"15000000\"}\n",
        ),
    )
    .unwrap();

    let out = rwa(&home)
        .args(["--json", "gm", "pnl"])
        .env("RWA_ONDO_API_URL", server.url("/assets"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let v = stdout_json(&out);
    assert_eq!(v["wallet"].as_str().unwrap(), pubkey);
    assert_eq!(v["trades_recorded"], 3);
    let t = &v["tokens"][0];
    assert_eq!(t["token"], "AALon");
    assert_eq!(t["qty"], "1");
    assert_eq!(t["avg_cost"], 12.0);
    assert_eq!(t["market_price"], 12.0);
    assert_eq!(t["invested_usdc"], 12.0);
    assert_eq!(t["unrealized_usdc"], 0.0);
    assert_eq!(t["realized_usdc"], 3.0);
    assert!(t.get("oversold_qty").is_none());
    assert_eq!(v["totals"]["invested_usdc"], 12.0);
    assert_eq!(v["totals"]["realized_usdc"], 3.0);
    assert_eq!(v["totals"]["total_pnl_usdc"], 3.0);
}
