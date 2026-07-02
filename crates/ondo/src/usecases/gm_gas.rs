//! Automatic SOL gas refueling: when the wallet runs low on SOL but holds
//! USDC, buy a small amount of SOL so fees and ATA rent never strand a
//! USDC-only wallet. Best-effort by design — every impossible-to-refuel case
//! degrades to "continue without" with a stderr note, and the route-aware SOL
//! gate on the main trade stays the safety authority.

use eyre::Result;

use super::gm_execute::{execute_with_retry, finalize_execution, SwapParams};
use super::gm_internal::get_order_checked;
use crate::types::Mint;
use crate::{amounts, jupiter, solana, wallet};

/// Refuel triggers below this SOL balance (0.003 SOL ≈ 1.5× the fee floor:
/// enough for one ATA rent + fees, but already "running on reserve").
pub const SOL_LOW_WATER_LAMPORTS: u64 = 3_000_000;
/// Refuel size in raw USDC — the trade minimum (5 USDC ≈ 0.02+ SOL: months of
/// fees and ~10 ATA creations).
pub const REFUEL_USDC_RAW: u128 = 5_000_000;
/// Below this the wallet can't even pay a single tx fee itself, so the
/// bootstrap swap must ride a gasless route (Jupiter as fee payer).
const BOOTSTRAP_LAMPORTS: u64 = 100_000;

/// A completed gas refuel, reported alongside the main operation's output.
pub struct GasRefuel {
    pub usdc_spent: String,
    pub sol_received: String,
    pub signature: String,
}

/// Pure decision: SOL below the low-water mark AND enough USDC to cover both
/// the upcoming operation (`reserved_usdc_raw`) and the refuel itself.
pub(crate) fn needs_refuel(sol_lamports: u64, usdc_raw: u128, reserved_usdc_raw: u128) -> bool {
    sol_lamports < SOL_LOW_WATER_LAMPORTS
        && usdc_raw >= reserved_usdc_raw.saturating_add(REFUEL_USDC_RAW)
}

/// Check balances and, with the caller's consent, top up SOL by swapping
/// `REFUEL_USDC_RAW` USDC → native SOL. `reserved_usdc_raw` is what the
/// upcoming operation itself needs (so the refuel never eats the trade's
/// funds). `consent` receives the current SOL balance and decides (CLI prompt
/// or auto). Returns `Ok(None)` whenever refueling is unnecessary, declined,
/// or impossible — the caller's operation proceeds either way.
pub async fn ensure_gas(
    w: &wallet::Wallet,
    rpc_url: Option<&str>,
    json: bool,
    reserved_usdc_raw: u128,
    consent: impl FnOnce(f64) -> bool,
) -> Result<Option<GasRefuel>> {
    let taker = w.pubkey();
    let (sol_res, usdc_res) = tokio::join!(
        solana::get_sol_balance_raw(&taker, rpc_url),
        solana::get_usdc_balance_raw(&taker, rpc_url),
    );
    let sol_lamports: u64 = sol_res?.parse().unwrap_or(0);
    let (_, usdc_raw_s) = usdc_res?;
    let usdc_raw: u128 = usdc_raw_s.parse().unwrap_or(0);

    if sol_lamports >= SOL_LOW_WATER_LAMPORTS {
        return Ok(None);
    }
    let sol_now = sol_lamports as f64 / 1_000_000_000.0;
    if !needs_refuel(sol_lamports, usdc_raw, reserved_usdc_raw) {
        eprintln!(
            "Note: SOL is low ({sol_now:.6}) but USDC can't cover a {} USDC gas refuel on top of this operation; continuing without.",
            REFUEL_USDC_RAW as f64 / 1_000_000.0
        );
        return Ok(None);
    }
    if !consent(sol_now) {
        return Ok(None);
    }

    match refuel(w, &taker, sol_lamports, json).await {
        Ok(refueled) => Ok(refueled),
        Err(e) => {
            // Best-effort: a failed refuel must not fail the user's trade —
            // the route-aware SOL gate will still refuse anything unsafe.
            eprintln!("Warning: SOL gas refuel failed ({e}); continuing without.");
            Ok(None)
        }
    }
}

async fn refuel(
    w: &wallet::Wallet,
    taker: &str,
    sol_lamports: u64,
    json: bool,
) -> Result<Option<GasRefuel>> {
    let refuel_raw = REFUEL_USDC_RAW.to_string();
    let (order, _slippage) = get_order_checked(
        jupiter::USDC_MINT,
        jupiter::SOL_MINT,
        &refuel_raw,
        taker,
        None,
        json,
        None,
    )
    .await?;

    // Bootstrap: with (almost) no SOL only a gasless route can execute — the
    // wallet cannot pay the swap's own fee.
    if sol_lamports < BOOTSTRAP_LAMPORTS && order.gasless != Some(true) {
        eprintln!(
            "Note: no gasless route for the SOL refuel right now and the wallet can't pay fees itself; continuing without."
        );
        return Ok(None);
    }

    let input_mint = Mint::from(jupiter::USDC_MINT);
    let output_mint = Mint::from(jupiter::SOL_MINT);
    let params = SwapParams {
        input_mint: &input_mint,
        output_mint: &output_mint,
        raw_amount: &refuel_raw,
        taker,
        slippage_bps: None,
    };
    let result = execute_with_retry(w, &order, json, &params).await?;
    let exec = finalize_execution(&order, &result, jupiter::GM_SOL_DECIMALS);
    Ok(Some(GasRefuel {
        usdc_spent: amounts::format_amount(&refuel_raw, jupiter::USDC_DECIMALS),
        sol_received: exec.output_amount,
        signature: exec.signature,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_refuel_matrix() {
        // Low SOL + enough USDC for trade (10) and refuel (5) → refuel.
        assert!(needs_refuel(0, 15_000_000, 10_000_000));
        assert!(needs_refuel(2_999_999, 15_000_000, 10_000_000));
        // SOL at/above the low-water mark → never.
        assert!(!needs_refuel(3_000_000, 100_000_000, 0));
        // USDC one unit short of trade + refuel → skip (trade keeps priority).
        assert!(!needs_refuel(0, 14_999_999, 10_000_000));
        // No trade reserved: 5 USDC exactly is enough.
        assert!(!needs_refuel(0, 4_999_999, 0));
        assert!(needs_refuel(0, 5_000_000, 0));
    }
}
