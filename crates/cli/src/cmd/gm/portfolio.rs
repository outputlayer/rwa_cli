use eyre::Result;
use rwa_ondo::{api, solana, token_list, usecases};

use super::*;

pub async fn portfolio(
    wallet_addr: Option<&str>,
    view_terms: &[String],
    json: bool,
    rpc_url: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let tokens = token_list::get_token_list();
    let pubkey = match wallet_addr {
        Some(w) => {
            solana::validate_address(w)?;
            w.to_string()
        }
        None => load_wallet(selected)?.pubkey(),
    };

    let (portfolio_bal, assets) = tokio::join!(
        usecases::gm::fetch_portfolio_balances(&pubkey, tokens, rpc_url),
        api::fetch_assets()
    );
    let portfolio_bal = portfolio_bal?;
    let assets = assets?;
    let balance_source = portfolio_bal.source;
    let sol_bal = portfolio_bal.sol;
    let usdc_bal = portfolio_bal.usdc;

    let summary = usecases::gm::compute_portfolio(&portfolio_bal.gm_tokens, &assets);
    if !json {
        for u in &summary.unavailable {
            eprintln!("  Skipping {} — {}", u.symbol, u.reason);
        }
    }
    let positions: Vec<PositionJson> = summary
        .positions
        .into_iter()
        .map(|p| {
            let asset = api::find_asset(&p.token, &assets);
            PositionJson {
                token: p.token,
                balance: p.balance,
                price: p.price,
                value_usd: p.value_usd,
                gm_alloc_pct: p.gm_alloc_pct,
                change_pct_24h: p.change_pct_24h,
                shares_per_token: p.shares_per_token,
                sector: asset.and_then(|a| a.sector()).map(String::from),
                asset_class: asset.and_then(|a| a.asset_class()).map(String::from),
                region: asset.and_then(|a| a.region()).map(String::from),
                kind: asset.and_then(|a| a.instrument_type()).map(String::from),
                tags: asset
                    .map(|a| a.tag_labels().map(String::from).collect())
                    .unwrap_or_default(),
                group_alloc_pct: None,
            }
        })
        .collect();
    let unavailable: Vec<PortfolioUnavailableJson> = summary
        .unavailable
        .into_iter()
        .map(|u| PortfolioUnavailableJson {
            symbol: u.symbol,
            reason: u.reason,
        })
        .collect();
    let gm_positions_value = summary.value_usd;
    let gm_positions_change = summary.change_24h_usd;
    let gm_positions_change_pct = summary.change_24h_pct;

    // `--view`: parse against the catalog we already fetched, then filter/split.
    // Parsing deliberately happens after the fetch — the catalog IS the
    // dictionary, and `portfolio` cannot run without it anyway (prices).
    //
    // `positions` is consumed exactly once here, in either arm, and rebound
    // to whichever position list downstream should use (unfiltered, or the
    // view's filtered subset) — so neither branch below has to reason about
    // a conditional move of the original vector. Widening the match's own
    // arms to build the *outputs* both branches need (instead of trying to
    // resurrect `positions` after a partial move inside a `Some`/`None`
    // wrapper) is what keeps this a rebind rather than a clone.
    let (positions, view, groups): (Vec<PositionJson>, Option<ViewJson>, Option<Vec<GroupJson>>) =
        if view_terms.is_empty() {
            (positions, None, None)
        } else {
            let spec = view::parse_view(view_terms, &assets)?;
            let (filtered, groups, overlapping) = view::apply_view(positions, &spec);
            let matched_value: f64 = filtered.iter().map(|p| p.value_usd).sum();
            let view_json = ViewJson {
                terms: spec.terms.clone(),
                filters: ViewFiltersJson {
                    tags: spec.tags.iter().map(|f| f.label.clone()).collect(),
                    tokens: spec.tokens.clone(),
                },
                split_by: spec.split.map(|c| c.term().to_string()),
                overlapping,
                matched_positions: filtered.len(),
                matched_value_usd: matched_value,
                matched_pct_of_gm: if gm_positions_value.abs() > f64::EPSILON {
                    matched_value / gm_positions_value * 100.0
                } else {
                    0.0
                },
            };
            (filtered, Some(view_json), Some(groups))
        };

    let source = if balance_source == solana::BalanceSource::Jupiter {
        if !json {
            eprintln!("Note: Solana RPC unavailable — balances via Jupiter holdings.");
        }
        Some("jupiter")
    } else {
        None
    };

    if json {
        return json_out(&PortfolioJson {
            wallet: pubkey.clone(),
            cash: PortfolioCashJson {
                sol: sol_bal,
                usdc: usdc_bal,
            },
            gm_positions: PortfolioGmPositionsJson {
                positions,
                value_usd: gm_positions_value,
                change_24h_usd: gm_positions_change,
                change_24h_pct: gm_positions_change_pct,
                view,
                groups,
            },
            unavailable,
            source,
        });
    }

    match (view, groups) {
        (Some(v), Some(groups)) => print!("{}", render_view(&pubkey, sol_bal, usdc_bal, &groups, &v,
                                                              gm_positions_value, gm_positions_change_pct)),
        _ => print!("{}", render_portfolio(&pubkey, sol_bal, usdc_bal, &positions,
                                            gm_positions_value, gm_positions_change_pct)),
    }
    Ok(())
}

/// Human-readable portfolio table. Pure: returns the block so it can be
/// asserted in tests instead of being printed straight to stdout.
fn render_portfolio(
    pubkey: &str,
    sol: f64,
    usdc: f64,
    positions: &[PositionJson],
    value_usd: f64,
    change_pct: f64,
) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "Portfolio for {pubkey}\n");
    let _ = writeln!(o, "  SOL:   {sol:.6}");
    let _ = writeln!(o, "  USDC:  {usdc:.2}");

    if positions.is_empty() {
        let _ = writeln!(o, "\nNo GM token positions.");
        return o;
    }

    let _ = writeln!(o);
    let _ = writeln!(o, "{:<10} {:>12} {:>10} {:>12} {:>8} {:>8}",
                     "TOKEN", "BALANCE", "PRICE", "VALUE", "GM %", "24h");
    let _ = writeln!(o, "{}", "-".repeat(64));
    for p in positions {
        let _ = writeln!(o, "{:<10} {:>12.4} {:>10.2} {:>11.2} {:>7.1}% {:>+7.2}%",
                         p.token, p.balance, p.price, p.value_usd, p.gm_alloc_pct, p.change_pct_24h);
    }
    let _ = writeln!(o, "{}", "-".repeat(64));
    let _ = writeln!(o, "{:<10} {:>12} {:>10} {:>11.2} {:>7} {:>+7.2}%",
                     "GM TOTAL", "", "", value_usd, "", change_pct);
    let _ = writeln!(o, "  Cash balances shown above are separate from GM position totals.");
    o
}

/// Human-readable rendering of a `--view` result. Three shapes in one function
/// because they share every invariant: GM % is always portfolio-relative, GM
/// TOTAL appears exactly once, and the echo line always names the terms.
fn render_view(
    pubkey: &str,
    sol: f64,
    usdc: f64,
    groups: &[GroupJson],
    view: &ViewJson,
    gm_total: f64,
    gm_change_pct: f64,
) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "Portfolio for {pubkey}\n");
    let _ = writeln!(o, "  SOL:   {sol:.6}");
    let _ = writeln!(o, "  USDC:  {usdc:.2}");

    let mut echo = format!("view: {}", view.terms.join(", "));
    if let Some(by) = &view.split_by {
        let _ = write!(echo, " · split by {by} · {} groups", groups.len());
    }
    let _ = write!(echo, " · {} positions · {:.1}% of GM", view.matched_positions, view.matched_pct_of_gm);
    if view.overlapping {
        let _ = write!(echo, " · positions appear in more than one group");
    }
    let _ = writeln!(o, "{echo}\n");

    if groups.is_empty() {
        let _ = writeln!(o, "No positions match this view.");
        let _ = writeln!(o, "{:<10} {:>36.2} {:>+16.2}%", "GM TOTAL", gm_total, gm_change_pct);
        return o;
    }

    let split = view.split_by.is_some();
    for g in groups {
        if let Some(name) = &g.group {
            let _ = writeln!(o, "\n═══ {name} ═══ {:.2} · {:.1}% of GM ═══", g.value_usd, g.gm_alloc_pct);
        }
        let pct_header = if split { "GRP %" } else { "GM %" };
        let _ = writeln!(o, "{:<10} {:>12} {:>10} {:>12} {:>8} {:>8}",
                         "TOKEN", "BALANCE", "PRICE", "VALUE", pct_header, "24h");
        let _ = writeln!(o, "{}", "-".repeat(64));
        for p in &g.positions {
            let pct = if split { p.group_alloc_pct.unwrap_or(0.0) } else { p.gm_alloc_pct };
            let _ = writeln!(o, "{:<10} {:>12.4} {:>10.2} {:>11.2} {:>7.1}% {:>+7.2}%",
                             p.token, p.balance, p.price, p.value_usd, pct, p.change_pct_24h);
        }
        let _ = writeln!(o, "{}", "-".repeat(64));
        if split {
            let _ = writeln!(o, "{:<10} {:>36.2} {:>8} {:>+7.2}%", "SUBTOTAL", g.value_usd, "100.0%", g.change_24h_pct);
        }
    }

    // MATCHED only when the filter actually cut something — otherwise it would
    // duplicate GM TOTAL line for line.
    if !split && view.matched_value_usd < gm_total - 0.005 {
        let _ = writeln!(o, "{:<10} {:>36.2} {:>7.1}% {:>+7.2}%",
                         "MATCHED", view.matched_value_usd, view.matched_pct_of_gm,
                         groups.first().map_or(0.0, |g| g.change_24h_pct));
    }
    let _ = writeln!(o, "{:<10} {:>36.2} {:>16}{:>+7.2}%", "GM TOTAL", gm_total, "", gm_change_pct);
    let _ = writeln!(o, "  Cash balances shown above are separate from GM position totals.");
    o
}

pub async fn history(symbol: &str, range: &str, json: bool) -> Result<()> {
    // Resolve the symbol against the token list BEFORE hitting the API:
    // an unknown symbol gets a typed `unknown_token` (with a did-you-mean
    // suggestion) instead of the raw upstream HTTP 400 dump — and the
    // resolver already owns the on-suffix normalization.
    let entry = rwa_ondo::symbol_resolve::resolve_token(symbol, token_list::get_token_list())?;
    let sym = entry.symbol.to_string();

    let candles = api::fetch_history(&sym, range).await?;
    if candles.is_empty() {
        return Err(eyre::eyre!(
            "No price history for {} (range: {})",
            sym,
            range
        ));
    }

    let first = candles
        .first()
        .ok_or_else(|| eyre::eyre!("Empty candle data"))?;
    let last = candles
        .last()
        .ok_or_else(|| eyre::eyre!("Empty candle data"))?;
    let high = candles
        .iter()
        .map(|c| c.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let change_pct = if first.open > 0.0 {
        (last.close - first.open) / first.open * 100.0
    } else {
        0.0
    };

    if json {
        return json_out(&HistoryJson {
            symbol: sym,
            range: range.to_uppercase(),
            candles: candles.len(),
            first: HistoryCandleJson {
                timestamp: first.timestamp,
                price: first.open,
            },
            last: HistoryCandleJson {
                timestamp: last.timestamp,
                price: last.close,
            },
            high,
            low,
            change_pct,
        });
    }

    println!("{} Price History ({})", sym, range.to_uppercase());
    println!("{}", "-".repeat(50));
    println!("  Period:    {} candles", candles.len());
    println!("  Open:      ${:.2}", first.open);
    println!("  Close:     ${:.2}", last.close);
    println!("  High:      ${:.2}", high);
    println!("  Low:       ${:.2}", low);
    println!("  Change:    {:+.2}%", change_pct);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(token: &str, value: f64, gm_pct: f64, group_pct: Option<f64>) -> PositionJson {
        PositionJson {
            token: token.to_string(),
            balance: 1.0,
            price: value,
            value_usd: value,
            gm_alloc_pct: gm_pct,
            change_pct_24h: 0.0,
            shares_per_token: None,
            sector: None,
            asset_class: None,
            region: None,
            kind: None,
            tags: vec![],
            group_alloc_pct: group_pct,
        }
    }

    fn view_json(terms: &[&str], split: Option<&str>, overlapping: bool, matched: usize, matched_value: f64, pct: f64) -> ViewJson {
        ViewJson {
            terms: terms.iter().map(|s| (*s).to_string()).collect(),
            filters: ViewFiltersJson { tags: vec![], tokens: vec![] },
            split_by: split.map(String::from),
            overlapping,
            matched_positions: matched,
            matched_value_usd: matched_value,
            matched_pct_of_gm: pct,
        }
    }

    /// breaks if: a subtotal line is ever labelled GM TOTAL — a reader (or a
    /// script scraping human output) would take a slice for the whole wallet.
    #[test]
    fn gm_total_appears_exactly_once_even_with_many_groups() {
        let groups = vec![
            GroupJson { group: Some("Value".into()), value_usd: 600.0, gm_alloc_pct: 60.0,
                        change_24h_pct: -1.0, positions: vec![position("Aon", 600.0, 60.0, Some(100.0))] },
            GroupJson { group: Some("Growth".into()), value_usd: 400.0, gm_alloc_pct: 40.0,
                        change_24h_pct: 2.0, positions: vec![position("Bon", 400.0, 40.0, Some(100.0))] },
        ];
        let out = render_view("WALLET", 1.0, 2.0, &groups,
                              &view_json(&["factor"], Some("factor"), true, 2, 1000.0, 100.0), 1000.0, 0.0);

        assert_eq!(out.matches("GM TOTAL").count(), 1, "exactly one GM TOTAL:\n{out}");
        assert_eq!(out.matches("SUBTOTAL").count(), 2, "one SUBTOTAL per group:\n{out}");
    }

    /// breaks if: the per-position percentage column collapses to one meaning
    /// regardless of split — SPLIT must render `GRP %` / `group_alloc_pct`,
    /// NO-SPLIT must render `GM %` / `gm_alloc_pct`. The fixture uses two
    /// clearly different numbers (42.5 vs 77.5) so the assertion reads the
    /// actual printed value, not just the header word — a mutation that
    /// always picks `GM %`/`gm_alloc_pct` regardless of `split` must fail
    /// this even though the header-only tests elsewhere stay green.
    #[test]
    fn split_prints_group_alloc_pct_no_split_prints_gm_alloc_pct() {
        let split_groups = vec![GroupJson {
            group: Some("Value".into()), value_usd: 600.0, gm_alloc_pct: 15.0, change_24h_pct: 0.0,
            positions: vec![position("Aon", 600.0, 42.5, Some(77.5))],
        }];
        let split = render_view("W", 0.0, 0.0, &split_groups,
                                &view_json(&["factor"], Some("factor"), false, 1, 600.0, 33.0), 600.0, 0.0);
        assert!(split.contains("GRP %"), "split render must head the column GRP %:\n{split}");
        assert!(split.contains("77.5%"), "split row must print group_alloc_pct (77.5):\n{split}");
        assert!(!split.contains("42.5%"), "split row must not leak gm_alloc_pct (42.5) into the pct column:\n{split}");

        let no_split_groups = vec![GroupJson {
            group: None, value_usd: 600.0, gm_alloc_pct: 15.0, change_24h_pct: 0.0,
            positions: vec![position("Aon", 600.0, 42.5, Some(77.5))],
        }];
        let no_split = render_view("W", 0.0, 0.0, &no_split_groups,
                                   &view_json(&["factor"], None, false, 1, 600.0, 33.0), 600.0, 0.0);
        assert!(no_split.contains("GM %"), "no-split render must head the column GM %:\n{no_split}");
        assert!(no_split.contains("42.5%"), "no-split row must print gm_alloc_pct (42.5):\n{no_split}");
        assert!(!no_split.contains("77.5%"), "no-split row must not leak group_alloc_pct (77.5) into the pct column:\n{no_split}");
    }

    /// breaks if: the overlap note is printed unconditionally — it would then
    /// read as a warning on views where sums are exact.
    #[test]
    fn overlap_note_appears_only_when_groups_overlap() {
        let groups = vec![GroupJson { group: Some("US".into()), value_usd: 1000.0, gm_alloc_pct: 100.0,
                                      change_24h_pct: 0.0, positions: vec![position("Aon", 1000.0, 100.0, Some(100.0))] }];

        let overlapping = render_view("W", 0.0, 0.0, &groups,
                                      &view_json(&["factor"], Some("factor"), true, 1, 1000.0, 100.0), 1000.0, 0.0);
        assert!(overlapping.contains("more than one"), "must warn when overlapping:\n{overlapping}");

        let clean = render_view("W", 0.0, 0.0, &groups,
                                &view_json(&["region"], Some("region"), false, 1, 1000.0, 100.0), 1000.0, 0.0);
        assert!(!clean.contains("more than one"), "must stay silent when exact:\n{clean}");
    }

    /// breaks if: MATCHED is printed when the filter kept everything (a line
    /// duplicating GM TOTAL) or omitted when it actually cut something.
    #[test]
    fn matched_line_appears_only_when_the_filter_cut_something() {
        let groups = vec![GroupJson { group: None, value_usd: 300.0, gm_alloc_pct: 30.0,
                                      change_24h_pct: 0.0, positions: vec![position("Aon", 300.0, 30.0, Some(100.0))] }];

        let partial = render_view("W", 0.0, 0.0, &groups,
                                  &view_json(&["Dividend"], None, false, 1, 300.0, 30.0), 1000.0, 0.0);
        assert!(partial.contains("MATCHED"), "partial match must show MATCHED:\n{partial}");

        let full = render_view("W", 0.0, 0.0, &groups,
                               &view_json(&["Healthcare"], None, false, 1, 1000.0, 100.0), 1000.0, 0.0);
        assert!(!full.contains("MATCHED"), "100% match must not duplicate the total:\n{full}");
    }

    /// breaks if: an empty view renders a phantom table instead of saying so.
    #[test]
    fn empty_view_states_no_match_and_still_shows_the_total() {
        let out = render_view("W", 0.0, 0.0, &[],
                              &view_json(&["Energy"], None, false, 0, 0.0, 0.0), 1000.0, -1.25);
        assert!(out.contains("No positions match"), "{out}");
        assert!(out.contains("GM TOTAL"), "the wallet total stays visible:\n{out}");
    }

    /// breaks if: the echo line is dropped — the polymorphic flag becomes a
    /// guessing game about whether a term was read as a tag or a category.
    #[test]
    fn the_view_echo_names_every_term() {
        let out = render_view("W", 0.0, 0.0, &[],
                              &view_json(&["Healthcare", "factor"], Some("factor"), false, 0, 0.0, 0.0), 1000.0, 0.0);
        assert!(out.contains("Healthcare"), "{out}");
        assert!(out.contains("factor"), "{out}");
    }

    /// breaks if: the pre-existing portfolio table loses a column or the cash
    /// disclaimer — this is the output that shipped before --view existed.
    #[test]
    fn plain_portfolio_render_keeps_its_columns_and_disclaimer() {
        let out = render_portfolio("WALLET", 26.8764, 1562.36,
                                   &[position("ABBVon", 1798.35, 20.77, None)], 1798.35, -1.39);
        for expected in ["TOKEN", "BALANCE", "PRICE", "VALUE", "GM %", "24h", "GM TOTAL", "ABBVon"] {
            assert!(out.contains(expected), "missing {expected}:\n{out}");
        }
        assert!(out.contains("separate from GM position totals"), "cash disclaimer:\n{out}");
    }
}
