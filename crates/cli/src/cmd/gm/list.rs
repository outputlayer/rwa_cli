use eyre::Result;
use rwa_ondo::{api, symbol_resolve, token_list};

use super::*;

struct ListContext {
    session: api::Session,
    items: Vec<ListItemJson>,
}

pub async fn hours(json: bool, show_tradable: bool) -> Result<()> {
    let now = api::now_eastern();

    let session = api::current_session();
    let closed = session == api::Session::Closed;

    let time_str = now.format("%A %I:%M %p ET").to_string();

    let next_session = api::next_session_start();
    let mins_left = (next_session - now).num_minutes().max(0);
    let countdown = if closed {
        format!("opens in {}h {}m", mins_left / 60, mins_left % 60)
    } else {
        format!("next session in {}h {}m", mins_left / 60, mins_left % 60)
    };

    // Fetched in every session: weekends/holidays map to Ondo's offhours
    // session, where select flagship tokens still trade 24/7. Both calls are
    // disk-cached (60s) and independent — fetch concurrently.
    let (limits_res, assets_res) = tokio::join!(
        api::fetch_session_limits(None),
        api::fetch_assets(),
    );

    let (tradable_count, tradable_list) = match limits_res {
        Ok(limits) => {
            let tradable: Vec<String> = limits.iter()
                .filter(|l| l.is_tradable(session))
                .map(|l| l.symbol.clone())
                .collect();
            let count = tradable.len();
            if show_tradable {
                (Some(count), Some(tradable))
            } else {
                (Some(count), None)
            }
        }
        Err(_) => (None, None),
    };

    // Asset metadata: paused (dividend window) and offhours-flagship counts.
    // Fail-open — on fetch error these fields are simply absent from JSON.
    let (paused_count, offhours_tradable_count, offhours_tradable_list) = match assets_res {
        Ok(assets) => {
            let paused = assets.iter().filter(|a| a.is_trading_paused).count();
            let offhours: Vec<String> = assets
                .iter()
                .filter(|a| a.is_offhours_tradable)
                .map(|a| a.symbol.clone())
                .collect();
            let offhours_count = offhours.len();
            if show_tradable {
                (Some(paused), Some(offhours_count), Some(offhours))
            } else {
                (Some(paused), Some(offhours_count), None)
            }
        }
        Err(_) => (None, None, None),
    };

    let status = if closed { "closed" } else { "open" };

    if json {
        return json_out(&HoursJson {
            status,
            session: session.label(),
            session_hours: session.hours(),
            now: time_str,
            countdown,
            next_session_at: next_session.timestamp(),
            tradable_count,
            tradable: tradable_list,
            paused_count,
            offhours_tradable_count,
            offhours_tradable: offhours_tradable_list,
        });
    }

    let label = if closed { "CLOSED" } else { "OPEN" };
    println!("{} — {}", label, session.label());
    println!("  Now:         {}", time_str);
    println!("  Session:     {} ({})", session.label(), session.hours());
    println!();
    println!("  Sessions:");
    println!("    Pre-Market:     4:00 AM – 9:29 AM ET");
    println!("    Regular:        9:30 AM – 3:59 PM ET");
    println!("    Post-Market:    4:00 PM – 7:59 PM ET");
    println!("    Overnight:      8:00 PM – 3:59 AM ET");
    println!("    Closed:         Weekend / NYSE holidays");
    println!();
    let cd = countdown.trim_start_matches("opens in ").trim_start_matches("next session in ");
    if closed {
        println!("  Opens in: {}", cd);
    } else {
        println!("  Next session in: {}", cd);
        if let Some(count) = tradable_count {
            println!("  Tradable now: {} tokens", count);
        }
    }
    if let (Some(offhours), Some(paused)) = (offhours_tradable_count, paused_count) {
        println!("  24/7 (offhours): {} flagship tokens; paused now: {}", offhours, paused);
    }
    Ok(())
}

pub async fn list(json: bool) -> Result<()> {
    search(json, &[], false, &[], None, &[], &[]).await
}

#[allow(clippy::too_many_arguments)]
pub async fn search(
    json: bool,
    search_terms: &[String],
    tradable_only: bool,
    sectors: &[String],
    kind: Option<&str>,
    name_keywords: &[String],
    tags: &[String],
) -> Result<()> {
    let context = fetch_list_context().await?;
    let filtered: Vec<ListItemJson> = context
        .items
        .into_iter()
        .filter(|item| matches_filters(item, search_terms, tradable_only, sectors, kind, name_keywords, tags))
        .collect();

    if json {
        return json_out(&filtered);
    }

    println!("{} GM tokens{}\n", filtered.len(), describe_filters(search_terms, tradable_only, sectors, kind, name_keywords));
    for item in &filtered {
        let classification = classification_label(item);
        let mark = if item.tradable { "✓" } else { "✗" };
        if classification.is_empty() {
            println!("  {} {:<12} {}", mark, item.symbol, item.name);
        } else {
            println!("  {} {:<12} {:<30} {}", mark, item.symbol, item.name, classification);
        }
    }
    Ok(())
}

pub async fn tradable(json: bool, symbols: &[String]) -> Result<()> {
    let context = fetch_list_context().await?;

    let items = if symbols.is_empty() {
        context
            .items
            .iter()
            .filter(|item| item.tradable)
            .map(|item| TradableItemJson {
                input: item.symbol.clone(),
                found: true,
                symbol: Some(item.symbol.clone()),
                name: Some(item.name.clone()),
                kind: Some(item.kind.clone()),
                sector: item.sector.clone(),
                tradable: true,
                trading_paused: item.trading_paused,
            })
            .collect::<Vec<_>>()
    } else {
        let item_map: std::collections::HashMap<String, &ListItemJson> = context
            .items
            .iter()
            .map(|item| (item.symbol.to_uppercase(), item))
            .collect();
        let tokens = token_list::get_token_list();
        symbols
            .iter()
            .map(|input| match symbol_resolve::resolve_token(input, tokens) {
                Ok(entry) => match item_map.get(&entry.symbol.to_uppercase()) {
                    Some(item) => TradableItemJson {
                        input: input.clone(),
                        found: true,
                        symbol: Some(item.symbol.clone()),
                        name: Some(item.name.clone()),
                        kind: Some(item.kind.clone()),
                        sector: item.sector.clone(),
                        tradable: item.tradable,
                        trading_paused: item.trading_paused,
                    },
                    None => TradableItemJson {
                        input: input.clone(),
                        found: false,
                        symbol: None,
                        name: None,
                        kind: None,
                        sector: None,
                        tradable: false,
                        trading_paused: false,
                    },
                },
                Err(_) => TradableItemJson {
                    input: input.clone(),
                    found: false,
                    symbol: None,
                    name: None,
                    kind: None,
                    sector: None,
                    tradable: false,
                    trading_paused: false,
                },
            })
            .collect::<Vec<_>>()
    };

    if json {
        return json_out(&TradableResultJson {
            session: context.session.label(),
            count: items.iter().filter(|item| item.tradable).count(),
            items,
        });
    }

    println!("Session: {}", context.session.label());
    for item in &items {
        if !item.found {
            println!("  ? {:<12} not found", item.input);
            continue;
        }
        let status = if item.tradable { "✓" } else { "✗" };
        println!(
            "  {} {:<12} {}",
            status,
            item.symbol.as_deref().unwrap_or(&item.input),
            item.name.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

async fn fetch_list_context() -> Result<ListContext> {
    let tokens = token_list::get_token_list();
    let session = api::current_session();
    // Two independent Ondo API calls (both disk-cached) — fetch concurrently.
    let (assets_res, limits_res) = tokio::join!(
        api::fetch_assets(),
        api::fetch_session_limits(None),
    );
    let assets = assets_res.unwrap_or_default();

    let tradable_set: std::collections::HashSet<String> = limits_res
        .unwrap_or_default()
        .iter()
        .filter(|l| l.is_tradable(session))
        .map(|l| l.symbol.to_uppercase())
        .collect();

    let asset_map: std::collections::HashMap<String, &api::OndoAsset> = assets
        .iter()
        .map(|a| (a.symbol.to_uppercase(), a))
        .collect();

    let items = tokens
        .iter()
        .map(|t| {
            let asset = asset_map.get(&t.symbol.to_uppercase());
            let name = asset.map(|a| clean_name(&a.asset_name)).unwrap_or_default();
            let kind = asset
                .and_then(|a| a.instrument_type())
                .unwrap_or_else(|| token_type_from_name(&name))
                .to_lowercase();
            let sector = asset.and_then(|a| a.sector()).map(String::from);
            let asset_class = asset.and_then(|a| a.asset_class()).map(String::from);
            let region = asset.and_then(|a| a.region()).map(String::from);
            let all_tags = asset
                .map(|a| a.tag_labels().collect::<Vec<_>>().join(" ").to_lowercase())
                .unwrap_or_default();
            let tradable = tradable_set.contains(&t.symbol.to_uppercase());
            let trading_paused = asset.is_some_and(|a| a.is_trading_paused);
            ListItemJson {
                symbol: t.symbol.to_string(),
                name,
                kind,
                sector,
                asset_class,
                region,
                tradable,
                trading_paused,
                all_tags,
            }
        })
        .collect();

    Ok(ListContext { session, items })
}

#[allow(clippy::too_many_arguments)]
fn matches_filters(
    item: &ListItemJson,
    search_terms: &[String],
    tradable_only: bool,
    sectors: &[String],
    kind: Option<&str>,
    name_keywords: &[String],
    tags: &[String],
) -> bool {
    if tradable_only && !item.tradable {
        return false;
    }

    if !search_terms.is_empty() {
        let symbol = item.symbol.to_lowercase();
        let name = item.name.to_lowercase();
        let matches_search = search_terms.iter().any(|term| {
            let term = term.to_lowercase();
            symbol.contains(&term) || name.contains(&term) || item.all_tags.contains(&term)
        });
        if !matches_search {
            return false;
        }
    }

    // `--tag` matches any Ondo tag label across all categories (sector,
    // asset class, region, factor/risk), substring, case-insensitive —
    // e.g. `--tag dividend`, `--tag asia`, `--tag "fixed income"`.
    if !tags.is_empty() {
        let matches_tag = tags
            .iter()
            .any(|t| item.all_tags.contains(&t.to_lowercase()));
        if !matches_tag {
            return false;
        }
    }

    if !sectors.is_empty() {
        let item_sector = item.sector.as_deref().unwrap_or("");
        let matches_sector = sectors
            .iter()
            .any(|sector| item_sector.eq_ignore_ascii_case(sector));
        if !matches_sector {
            return false;
        }
    }

    if let Some(kind) = kind
        && !item.kind.eq_ignore_ascii_case(kind)
    {
        return false;
    }

    if !name_keywords.is_empty() {
        let name = item.name.to_lowercase();
        let matches_keyword = name_keywords
            .iter()
            .any(|keyword| name.contains(&keyword.to_lowercase()));
        if !matches_keyword {
            return false;
        }
    }

    true
}

/// Display classification for a token row: stocks show their sector; ETFs
/// (usually sector-less) fall back to `asset class · region` so nothing lists
/// unclassified. Kept as a pure helper so every fallback arm is unit-testable
/// without a network fetch.
fn classification_label(item: &ListItemJson) -> String {
    match (&item.sector, &item.asset_class, &item.region) {
        (Some(s), _, _) => s.clone(),
        (None, Some(c), Some(r)) => format!("{c} · {r}"),
        (None, Some(c), None) => c.clone(),
        (None, None, Some(r)) => r.clone(),
        (None, None, None) => String::new(),
    }
}

fn describe_filters(
    search_terms: &[String],
    tradable_only: bool,
    sectors: &[String],
    kind: Option<&str>,
    name_keywords: &[String],
) -> String {
    let mut parts = Vec::new();
    if !search_terms.is_empty() {
        parts.push(format!("search={}", search_terms.join(", ")));
    }
    if tradable_only {
        parts.push("tradable_only".to_string());
    }
    if !sectors.is_empty() {
        parts.push(format!("sector={}", sectors.join(", ")));
    }
    if let Some(kind) = kind {
        parts.push(format!("type={kind}"));
    }
    if !name_keywords.is_empty() {
        parts.push(format!("name_keyword={}", name_keywords.join(", ")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item() -> ListItemJson {
        ListItemJson {
            symbol: "LMTon".to_string(),
            name: "Lockheed Martin".to_string(),
            kind: "stock".to_string(),
            sector: Some("Industrials".to_string()),
            asset_class: Some("Equities".to_string()),
            region: Some("US".to_string()),
            tradable: true,
            trading_paused: false,
            all_tags: "industrials equities us large cap value".to_string(),
        }
    }

    #[test]
    fn matches_filters_accepts_bulk_name_keyword_and_sector() {
        let item = sample_item();
        assert!(matches_filters(
            &item,
            &[],
            true,
            &["Industrials".to_string()],
            Some("stock"),
            &["lockheed".to_string(), "raytheon".to_string()],
            &[],
        ));
    }

    #[test]
    fn matches_filters_rejects_wrong_sector() {
        let item = sample_item();
        assert!(!matches_filters(
            &item,
            &[],
            false,
            &["Energy".to_string()],
            None,
            &[],
            &[],
        ));
    }

    #[test]
    fn matches_filters_accepts_multi_search() {
        let item = sample_item();
        assert!(matches_filters(
            &item,
            &["energy".to_string(), "lockheed".to_string()],
            false,
            &[],
            None,
            &[],
            &[],
        ));
    }

    #[test]
    fn tag_filter_matches_any_ondo_tag_category() {
        let item = sample_item();
        // Factor/risk, asset-class, and region labels all match --tag,
        // substring and case-insensitive.
        assert!(matches_filters(&item, &[], false, &[], None, &[], &["Large Cap".to_string()]));
        assert!(matches_filters(&item, &[], false, &[], None, &[], &["equities".to_string()]));
        assert!(matches_filters(&item, &[], false, &[], None, &[], &["value".to_string()]));
        assert!(!matches_filters(&item, &[], false, &[], None, &[], &["asia".to_string()]));
        // --search also reaches all tags now (e.g. searching by factor).
        assert!(!matches_filters(&item, &["dividend".to_string()], false, &[], None, &[], &[]));
        assert!(matches_filters(&item, &["large cap".to_string()], false, &[], None, &[], &[]));
    }

    /// User task: "Script `gm list`/`gm tradable` output — a paused (ex-dividend)
    /// asset must be visibly flagged, but a normal asset's JSON shouldn't grow a
    /// noisy `trading_paused: false` on every single row." Known gap: neither
    /// JSON shape had a serialization test before this.
    #[test]
    fn list_item_json_omits_trading_paused_unless_true() {
        let normal = sample_item(); // trading_paused: false
        let json = serde_json::to_value(&normal).unwrap();
        assert!(json.get("trading_paused").is_none(), "false must be omitted: {json}");

        let mut paused = sample_item();
        paused.trading_paused = true;
        let json = serde_json::to_value(&paused).unwrap();
        assert_eq!(json["trading_paused"], serde_json::json!(true));
    }

    #[test]
    fn classification_prefers_sector_for_stocks() {
        // A stock with a sector shows the sector, ignoring asset_class/region.
        let item = sample_item(); // sector: Industrials, class: Equities, region: US
        assert_eq!(classification_label(&item), "Industrials");
    }

    #[test]
    fn classification_falls_back_to_asset_class_and_region_for_sectorless_etf() {
        // Sector-less ETF: "asset class · region" so nothing lists unclassified.
        let etf = ListItemJson {
            sector: None,
            asset_class: Some("Fixed Income".to_string()),
            region: Some("Asia".to_string()),
            ..sample_item()
        };
        assert_eq!(classification_label(&etf), "Fixed Income · Asia");
    }

    #[test]
    fn classification_uses_asset_class_alone_when_region_absent() {
        let etf = ListItemJson {
            sector: None,
            asset_class: Some("Commodities".to_string()),
            region: None,
            ..sample_item()
        };
        assert_eq!(classification_label(&etf), "Commodities");
    }

    #[test]
    fn classification_uses_region_alone_when_class_absent() {
        let etf = ListItemJson {
            sector: None,
            asset_class: None,
            region: Some("Europe".to_string()),
            ..sample_item()
        };
        assert_eq!(classification_label(&etf), "Europe");
    }

    #[test]
    fn classification_is_empty_when_fully_unclassified() {
        let bare = ListItemJson {
            sector: None,
            asset_class: None,
            region: None,
            ..sample_item()
        };
        assert_eq!(classification_label(&bare), "");
    }

    #[test]
    fn describe_filters_joins_all_active_parts() {
        let out = describe_filters(
            &["tesla".to_string()],
            true,
            &["Technology".to_string()],
            Some("stock"),
            &["motors".to_string()],
        );
        assert_eq!(
            out,
            " (search=tesla; tradable_only; sector=Technology; type=stock; name_keyword=motors)"
        );
    }

    #[test]
    fn describe_filters_is_empty_with_no_filters() {
        assert_eq!(describe_filters(&[], false, &[], None, &[]), "");
    }

    #[test]
    fn tradable_item_json_omits_trading_paused_unless_true() {
        let normal = TradableItemJson {
            input: "SPY".to_string(),
            found: true,
            symbol: Some("SPYon".to_string()),
            name: Some("SPDR S&P 500 ETF".to_string()),
            kind: Some("etf".to_string()),
            sector: None,
            tradable: true,
            trading_paused: false,
        };
        let json = serde_json::to_value(&normal).unwrap();
        assert!(json.get("trading_paused").is_none(), "false must be omitted: {json}");

        let paused = TradableItemJson { trading_paused: true, ..normal };
        let json = serde_json::to_value(&paused).unwrap();
        assert_eq!(json["trading_paused"], serde_json::json!(true));
    }
}
