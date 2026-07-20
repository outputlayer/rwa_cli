//! Automatic SOL gas refueling: when the wallet runs low on SOL but holds
//! USDC, buy a small amount of SOL so fees and ATA rent never strand a
//! USDC-only wallet. Best-effort by design — every impossible-to-refuel case
//! degrades to "continue without" with a stderr note, and the route-aware SOL
//! gate on the main trade stays the safety authority.

use eyre::Result;

use super::gm::DEFAULT_SLIPPAGE_BPS;
use super::gm_execute::{execute_with_retry, finalize_execution, SwapParams};
use super::gm_internal::get_order_checked;
use crate::types::Mint;
use crate::{amounts, jupiter, solana, wallet};

/// Refuel triggers below this SOL balance (0.003 SOL ≈ 1.5× the fee floor:
/// enough for one ATA rent + fees, but already "running on reserve").
pub const SOL_LOW_WATER_LAMPORTS: u64 = 3_000_000;
/// Minimum refuel size in raw USDC — our own trade minimum. The actual size is
/// dynamic (see `gas_target_lamports`/`refuel_usdc_raw_for`), bounded to
/// [5, 25] USDC.
pub const REFUEL_USDC_RAW: u128 = 5_000_000;
/// Cap on a single refuel, so a fee spike or price glitch can never spend an
/// outsized chunk of the wallet on gas.
pub const REFUEL_USDC_MAX_RAW: u128 = 25_000_000;
/// The gas target covers this many simple transactions...
const TARGET_TXS: u64 = 50;
/// ...plus this many new token-account creations (ATA rent dominates).
const TARGET_NEW_ATAS: u64 = 5;
/// Below this the wallet can't even pay a single tx fee itself, so the
/// bootstrap swap must ride a gasless route (Jupiter as fee payer).
const BOOTSTRAP_LAMPORTS: u64 = 100_000;

/// A completed gas refuel, reported alongside the main operation's output.
pub struct GasRefuel {
    pub usdc_spent: String,
    pub sol_received: String,
    pub signature: String,
}

/// Wallet balances sampled by the auto-gas gate. Returned to the caller so
/// the subsequent preflight can reuse them instead of refetching — ONE wallet
/// state sample per command. Invalidated (None) whenever a refuel executed,
/// because the swap changed both balances.
#[derive(Clone, Copy, Debug)]
pub struct BalanceSnapshot {
    pub sol_lamports: u64,
    pub usdc_raw: u128,
}

/// Pure decision: SOL below the low-water mark AND enough USDC to cover both
/// the upcoming operation (`reserved_usdc_raw`) and the minimum refuel.
pub(crate) fn needs_refuel(sol_lamports: u64, usdc_raw: u128, reserved_usdc_raw: u128) -> bool {
    sol_lamports < SOL_LOW_WATER_LAMPORTS
        && usdc_raw >= reserved_usdc_raw.saturating_add(REFUEL_USDC_RAW)
}

/// Dynamic gas target at CURRENT network conditions: enough lamports for
/// `TARGET_TXS` transfers at the live fee estimate plus `TARGET_NEW_ATAS`
/// token-account rents, doubled as a safety buffer against fee/price drift.
pub(crate) fn gas_target_lamports(fee_per_tx: u64, ata_rent: u64) -> u64 {
    2 * (TARGET_TXS.saturating_mul(fee_per_tx) + TARGET_NEW_ATAS.saturating_mul(ata_rent))
}

/// USDC to spend to buy `missing_lamports`, priced from the probe quote
/// (`REFUEL_USDC_RAW` → `probe_out_lamports`). Clamped to [5, 25] USDC and to
/// what remains after the main operation; `None` when even the 5 USDC minimum
/// doesn't fit (or the probe is degenerate).
pub(crate) fn refuel_usdc_raw_for(
    missing_lamports: u64,
    probe_out_lamports: u64,
    usdc_available_after_op: u128,
) -> Option<u128> {
    if probe_out_lamports == 0 || usdc_available_after_op < REFUEL_USDC_RAW {
        return None;
    }
    let needed = (missing_lamports as u128)
        .saturating_mul(REFUEL_USDC_RAW)
        .div_ceil(probe_out_lamports as u128);
    let clamped = needed.clamp(REFUEL_USDC_RAW, REFUEL_USDC_MAX_RAW);
    Some(clamped.min(usdc_available_after_op))
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
) -> Result<(Option<GasRefuel>, Option<BalanceSnapshot>)> {
    let taker = w.pubkey();
    let (sol_res, usdc_res) = tokio::join!(
        solana::get_sol_balance_raw(&taker, rpc_url),
        solana::get_usdc_balance_raw(&taker, rpc_url),
    );
    let sol_lamports: u64 = sol_res?.parse().unwrap_or(0);
    let (_, usdc_raw_s) = usdc_res?;
    let usdc_raw: u128 = usdc_raw_s.parse().unwrap_or(0);
    let snapshot = BalanceSnapshot { sol_lamports, usdc_raw };

    if sol_lamports >= SOL_LOW_WATER_LAMPORTS {
        return Ok((None, Some(snapshot)));
    }
    let sol_now = sol_lamports as f64 / 1_000_000_000.0;
    if !needs_refuel(sol_lamports, usdc_raw, reserved_usdc_raw) {
        eprintln!(
            "Note: SOL is low ({sol_now:.6}) but USDC can't cover a {} USDC gas refuel on top of this operation; continuing without.",
            REFUEL_USDC_RAW as f64 / 1_000_000.0
        );
        return Ok((None, Some(snapshot)));
    }
    if !consent(sol_now) {
        return Ok((None, Some(snapshot)));
    }

    let usdc_after_op = usdc_raw.saturating_sub(reserved_usdc_raw);
    match refuel(w, &taker, sol_lamports, usdc_after_op, rpc_url, json).await {
        // A refuel that executed changed both balances — the snapshot is
        // stale, so the caller must refetch (fail closed on staleness).
        Ok(Some(refueled)) => Ok((Some(refueled), None)),
        Ok(None) => Ok((None, Some(snapshot))),
        Err(e) => {
            // Best-effort: a failed refuel must not fail the user's trade —
            // the route-aware SOL gate will still refuse anything unsafe.
            eprintln!("Warning: SOL gas refuel failed ({e}); continuing without.");
            Ok((None, Some(snapshot)))
        }
    }
}

async fn refuel(
    w: &wallet::Wallet,
    taker: &str,
    sol_lamports: u64,
    usdc_available_after_op: u128,
    rpc_url: Option<&str>,
    json: bool,
) -> Result<Option<GasRefuel>> {
    // Size the refuel from live network conditions: fee estimate + ATA rent
    // set the lamports target; the probe quote's implied SOL price converts
    // the missing amount to USDC.
    let (fee_per_tx, ata_rent) = tokio::join!(
        solana::estimate_tx_fee_lamports(rpc_url),
        solana::ata_rent_lamports(rpc_url),
    );
    let target = gas_target_lamports(fee_per_tx, ata_rent);
    let missing = target.saturating_sub(sol_lamports);

    // Never omit the request slippage (same rule as the trade paths): the
    // response echo feeds the pre-sign under-delivery floor.
    let probe_raw = REFUEL_USDC_RAW.to_string();
    let (probe, _slippage) = get_order_checked(
        jupiter::USDC_MINT,
        jupiter::SOL_MINT,
        &probe_raw,
        taker,
        Some(DEFAULT_SLIPPAGE_BPS),
        json,
        None,
    )
    .await?;
    let probe_out: u64 = probe.out_amount.parse().unwrap_or(0);
    let Some(spend_raw) = refuel_usdc_raw_for(missing, probe_out, usdc_available_after_op) else {
        eprintln!("Note: cannot size a SOL refuel right now; continuing without.");
        return Ok(None);
    };

    // Reuse the probe order when the minimum suffices; otherwise requote at
    // the computed size.
    let refuel_raw = spend_raw.to_string();
    let order = if spend_raw == REFUEL_USDC_RAW {
        probe
    } else {
        let (order, _s) = get_order_checked(
            jupiter::USDC_MINT,
            jupiter::SOL_MINT,
            &refuel_raw,
            taker,
            Some(DEFAULT_SLIPPAGE_BPS),
            json,
            None,
        )
        .await?;
        order
    };

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
        slippage_bps: Some(DEFAULT_SLIPPAGE_BPS),
    };
    let result = execute_with_retry(w, &order, json, &params).await?;
    let exec = finalize_execution(&order, &result, jupiter::GM_SOL_DECIMALS);
    crate::ledger::record(
        taker,
        &crate::ledger::LedgerEvent::now(
            Some(exec.signature.clone()),
            "gas_refuel",
            "SOL",
            &exec.output_amount_raw,
            Some(refuel_raw.clone()),
        ),
    );
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
    fn gas_target_scales_with_live_network_conditions() {
        // 50 txs × 15_000 lamports + 5 ATAs × 2_039_280 rent, doubled:
        // 2 × (750_000 + 10_196_400) = 21_892_800 (hand-derived).
        assert_eq!(gas_target_lamports(15_000, 2_039_280), 21_892_800);
        // Fee spike 10×: target grows accordingly — dynamic, not fixed.
        assert_eq!(gas_target_lamports(150_000, 2_039_280), 2 * (7_500_000 + 10_196_400));
    }

    #[test]
    fn refuel_size_follows_sol_price_within_bounds() {
        // Cheap SOL (probe: 5 USDC → 0.025 SOL): the minimum already covers
        // the target → clamp up to the 5 USDC floor.
        assert_eq!(
            refuel_usdc_raw_for(21_892_800, 25_000_000, 100_000_000),
            Some(REFUEL_USDC_RAW)
        );
        // Expensive SOL (probe: 5 USDC → 0.008 SOL, ≈$625/SOL): need more —
        // ceil(21_892_800 × 5M / 8M) = 13_683_000 raw (13.68 USDC).
        assert_eq!(
            refuel_usdc_raw_for(21_892_800, 8_000_000, 100_000_000),
            Some(13_683_000)
        );
        // Absurd fee spike: capped at 25 USDC, never drains the wallet.
        assert_eq!(
            refuel_usdc_raw_for(100_000_000, 8_000_000, 100_000_000),
            Some(REFUEL_USDC_MAX_RAW)
        );
        // Only 6 USDC left after the trade: spend what's available.
        assert_eq!(
            refuel_usdc_raw_for(21_892_800, 8_000_000, 6_000_000),
            Some(6_000_000)
        );
        // Less than the 5 USDC minimum left, or a degenerate probe: no refuel.
        assert_eq!(refuel_usdc_raw_for(21_892_800, 8_000_000, 4_999_999), None);
        assert_eq!(refuel_usdc_raw_for(21_892_800, 0, 100_000_000), None);
    }

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
