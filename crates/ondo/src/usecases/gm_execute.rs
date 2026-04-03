use eyre::{Result, WrapErr};

use crate::{amounts, jupiter, wallet};
use crate::types::Mint;
use super::gm::{SwapExecution, SellOrderReady, BuyOrderReady, DEFAULT_SLIPPAGE_BPS};
use super::gm_internal::{get_order_checked, MAX_SWAP_RETRIES};

pub(crate) struct SwapParams<'a> {
    pub(crate) input_mint: &'a Mint,
    pub(crate) output_mint: &'a Mint,
    pub(crate) raw_amount: &'a str,
    pub(crate) taker: &'a str,
    pub(crate) slippage_bps: Option<u32>,
}

pub async fn execute_sell_raw(
    wallet: &wallet::Wallet,
    mint: &str,
    raw_amount: &str,
    taker: &str,
    json: bool,
) -> Result<SwapExecution> {
    let (order, _) = get_order_checked(
        mint,
        jupiter::USDC_MINT,
        raw_amount,
        taker,
        None,
        json,
        None,
    )
    .await?;
    let input_mint = Mint::from(mint);
    let output_mint = Mint::from(jupiter::USDC_MINT);
    let params = SwapParams {
        input_mint: &input_mint,
        output_mint: &output_mint,
        raw_amount,
        taker,
        slippage_bps: None,
    };
    let result = execute_with_retry(wallet, &order, json, &params).await?;
    let actual_slippage_pct = calc_actual_slippage(&order, &result);
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, jupiter::USDC_DECIMALS))
            .unwrap_or_else(|| amounts::format_amount(&order.out_amount, jupiter::USDC_DECIMALS)),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
        actual_slippage_pct,
    })
}

/// Phase 2 for parallel close-all: execute a pre-fetched sell order.
pub async fn execute_sell_from_order(
    wallet: &wallet::Wallet,
    ready: SellOrderReady,
    json: bool,
) -> Result<SwapExecution> {
    let input_mint = Mint::from(ready.mint.as_str());
    let output_mint = Mint::from(jupiter::USDC_MINT);
    // taker is encoded in the signed tx; for RefreshOrder retry we need it separately
    // We grab it from the wallet since SellOrderReady is always for our own wallet.
    let taker = wallet.pubkey();
    let params = SwapParams {
        input_mint: &input_mint,
        output_mint: &output_mint,
        raw_amount: &ready.raw_amount,
        taker: &taker,
        slippage_bps: None,
    };
    let result = execute_with_retry(wallet, &ready.order, json, &params).await?;
    let actual_slippage_pct = calc_actual_slippage(&ready.order, &result);
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, jupiter::USDC_DECIMALS))
            .unwrap_or_else(|| amounts::format_amount(&ready.order.out_amount, jupiter::USDC_DECIMALS)),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
        actual_slippage_pct,
    })
}

/// Phase 2 for basket buy: execute a pre-fetched buy order.
pub async fn execute_buy_from_order(
    wallet: &wallet::Wallet,
    ready: BuyOrderReady,
    json: bool,
) -> Result<SwapExecution> {
    let input_mint = Mint::from(jupiter::USDC_MINT);
    let output_mint = ready.gm_mint;
    let taker = wallet.pubkey();
    let params = SwapParams {
        input_mint: &input_mint,
        output_mint: &output_mint,
        raw_amount: &ready.usdc_raw,
        taker: &taker,
        slippage_bps: Some(DEFAULT_SLIPPAGE_BPS),
    };
    let result = execute_with_retry(wallet, &ready.order, json, &params).await?;
    let actual_slippage_pct = calc_actual_slippage(&ready.order, &result);
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, jupiter::GM_SOL_DECIMALS))
            .unwrap_or_else(|| {
                amounts::format_amount(&ready.order.out_amount, jupiter::GM_SOL_DECIMALS)
            }),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
        actual_slippage_pct,
    })
}

pub(crate) async fn execute_with_retry(
    wallet: &wallet::Wallet,
    order: &jupiter::OrderResponse,
    json: bool,
    params: &SwapParams<'_>,
) -> Result<jupiter::ExecuteResponse> {
    let mut current_order_owned: Option<jupiter::OrderResponse> = None;
    let mut last_err = None;

    for attempt in 0..=MAX_SWAP_RETRIES {
        let ord = current_order_owned.as_ref().unwrap_or(order);
        let execute_result = jupiter::execute_order(wallet, ord).await;
        match execute_result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let retry_action = e
                    .downcast_ref::<jupiter::ExecuteFailure>()
                    .map(|failure| failure.kind.retry_action())
                    .unwrap_or(jupiter::ExecuteRetryAction::None);

                if retry_action == jupiter::ExecuteRetryAction::None || attempt == MAX_SWAP_RETRIES {
                    return Err(e);
                }
                if !json {
                    eprintln!(
                        "Transient error (attempt {}/{}): {e}, retrying in 3s...",
                        attempt + 1,
                        MAX_SWAP_RETRIES
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if retry_action == jupiter::ExecuteRetryAction::RefreshOrder {
                    current_order_owned = Some(
                        jupiter::get_order(
                            None,
                            params.input_mint,
                            params.output_mint,
                            params.raw_amount,
                            params.taker,
                            params.slippage_bps,
                        )
                        .await
                        .wrap_err("failed to refresh quote after transient error")?,
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| eyre::eyre!("Swap failed after retries")))
}

/// Compute actual slippage from execute response amounts vs order quote.
/// Returns `None` if the execute response omits the result amounts.
pub(crate) fn calc_actual_slippage(
    order: &jupiter::OrderResponse,
    exec: &jupiter::ExecuteResponse,
) -> Option<f64> {
    let actual_in: u128 = exec.input_amount_result.as_deref()?.parse().ok()?;
    let actual_out: u128 = exec.output_amount_result.as_deref()?.parse().ok()?;
    let quoted_in: u128 = order.in_amount.parse().ok()?;
    let quoted_out: u128 = order.out_amount.parse().ok()?;
    if quoted_in == 0 || quoted_out == 0 {
        return None;
    }
    // actual_ratio = actual_out / actual_in
    // quoted_ratio = quoted_out / quoted_in
    // slippage = (actual_ratio / quoted_ratio - 1) * 100
    let actual_ratio = actual_out as f64 / actual_in as f64;
    let quoted_ratio = quoted_out as f64 / quoted_in as f64;
    Some((actual_ratio / quoted_ratio - 1.0) * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_actual_slippage_zero_when_exact_match() {
        let order = jupiter::OrderResponse {
            in_amount: "1000".into(),
            out_amount: "2000".into(),
            ..Default::default()
        };
        let exec = jupiter::ExecuteResponse {
            input_amount_result: Some("1000".into()),
            output_amount_result: Some("2000".into()),
            ..Default::default()
        };
        let slip = calc_actual_slippage(&order, &exec);
        assert!(slip.is_some());
        assert!((slip.unwrap()).abs() < 0.001);
    }

    #[test]
    fn calc_actual_slippage_negative_when_got_less() {
        let order = jupiter::OrderResponse {
            in_amount: "1000".into(),
            out_amount: "1000".into(),
            ..Default::default()
        };
        // Got 990 out instead of 1000 → -1% slippage
        let exec = jupiter::ExecuteResponse {
            input_amount_result: Some("1000".into()),
            output_amount_result: Some("990".into()),
            ..Default::default()
        };
        let slip = calc_actual_slippage(&order, &exec).unwrap();
        assert!((slip - (-1.0)).abs() < 0.01, "got {slip}");
    }

    #[test]
    fn calc_actual_slippage_none_when_exec_missing_amounts() {
        let order = jupiter::OrderResponse {
            in_amount: "1000".into(),
            out_amount: "1000".into(),
            ..Default::default()
        };
        let exec = jupiter::ExecuteResponse {
            input_amount_result: None,
            output_amount_result: None,
            ..Default::default()
        };
        assert!(calc_actual_slippage(&order, &exec).is_none());
    }

    #[test]
    fn calc_actual_slippage_none_when_zero_quoted_in() {
        let order = jupiter::OrderResponse {
            in_amount: "0".into(),
            out_amount: "1000".into(),
            ..Default::default()
        };
        let exec = jupiter::ExecuteResponse {
            input_amount_result: Some("0".into()),
            output_amount_result: Some("1000".into()),
            ..Default::default()
        };
        assert!(calc_actual_slippage(&order, &exec).is_none());
    }
}
