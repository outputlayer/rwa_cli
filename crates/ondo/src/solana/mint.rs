use eyre::{Result, eyre};
use serde::Deserialize;

use super::rpc::{rpc_call_simple, RpcMode};

/// Pure: pull the Scaled-UI multiplier out of a `getAccountInfo` (jsonParsed)
/// mint account value. `None` when the mint carries no scaledUiAmountConfig.
fn extract_mint_multiplier(account_value: &serde_json::Value) -> Option<f64> {
    account_value
        .get("data")?
        .get("parsed")?
        .get("info")?
        .get("extensions")?
        .as_array()?
        .iter()
        .find(|e| e.get("extension").and_then(|v| v.as_str()) == Some("scaledUiAmountConfig"))?
        .get("state")?
        .get("multiplier")
        .and_then(|m| m.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| m.as_f64()))
}

/// Scaled-UI multiplier for a mint (underlying shares per raw token).
/// `Ok(None)` = mint account exists but carries no scaledUiAmountConfig
/// extension (semantically 1). `Err` = RPC failure, or the mint account
/// itself was not found — callers that REQUIRE the multiplier (share-frame
/// limits) must fail closed on this rather than silently assume 1.
pub async fn get_mint_multiplier(mint: &str, rpc_url: Option<&str>) -> Result<Option<f64>> {
    #[derive(Deserialize)]
    struct AccountInfoResult {
        value: Option<serde_json::Value>,
    }
    let result: AccountInfoResult = rpc_call_simple(
        "getAccountInfo",
        serde_json::json!([mint, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        rpc_url,
        RpcMode::Race,
    )
    .await?;
    let Some(account_value) = result.value else {
        return Err(eyre!("mint account {mint} not found — cannot read scaled-UI multiplier"));
    };
    Ok(extract_mint_multiplier(&account_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mint_multiplier_from_account_info() {
        // Shape captured live from SPYon's mint (2026-07-06).
        let value = serde_json::json!({
            "data": { "parsed": { "info": {
                "decimals": 9,
                "extensions": [
                    { "extension": "somethingElse", "state": {} },
                    { "extension": "scaledUiAmountConfig", "state": {
                        "authority": "9foMHsSDq7nMg4WPusSz9eY7tyxyukqborA8GyU5cUxD",
                        "multiplier": "1.0077209101501272",
                        "newMultiplier": "1.0077209101501272",
                        "newMultiplierEffectiveTimestamp": 1781741045u64
                    }}
                ]
            }}}
        });
        let m = extract_mint_multiplier(&value).unwrap();
        assert!((m - 1.0077209101501272).abs() < 1e-12);
        // No extension → None (multiplier 1 semantics decided by callers).
        let plain = serde_json::json!({ "data": { "parsed": { "info": { "decimals": 9 } } } });
        assert_eq!(extract_mint_multiplier(&plain), None);
    }

    #[tokio::test]
    async fn get_mint_multiplier_fails_closed_when_mint_account_not_found() {
        use httpmock::prelude::*;

        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": { "value": null } }));
        }).await;

        let url = server.base_url();
        let err = get_mint_multiplier("NonexistentMint1111111111111111111111111111", Some(&url))
            .await
            .expect_err("missing mint account must fail closed, not resolve to multiplier 1");
        assert!(err.to_string().contains("not found"), "unexpected error: {err}");
    }
}
