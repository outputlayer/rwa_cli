use chrono::Datelike;
use eyre::Result;
use rwa_ondo::{api, token_list};

use super::*;

pub async fn hours(json: bool, show_tradable: bool) -> Result<()> {
    use chrono::Timelike;
    use chrono_tz::US::Eastern;

    let now = chrono::Utc::now().with_timezone(&Eastern);
    let wd = now.weekday();
    let hour = now.hour();
    let min = now.minute();

    let session = api::current_session();
    let closed = session == api::Session::Closed;

    let time_str = now.format("%A %I:%M %p ET").to_string();

    let countdown = if closed {
        let days_until_sun: u32 = match wd {
            chrono::Weekday::Fri => 2,
            chrono::Weekday::Sat => 1,
            chrono::Weekday::Sun => 0,
            _ => 0,
        };
        let mins_left = if wd == chrono::Weekday::Sun {
            (20u32).saturating_sub(hour) * 60 - min
        } else {
            let to_midnight = (24 - hour) * 60 - min;
            let full_days = days_until_sun.saturating_sub(1);
            to_midnight + full_days * 24 * 60 + 20 * 60
        };
        format!("opens in {}h {}m", mins_left / 60, mins_left % 60)
    } else {
        let session_end_hour: u32 = match session {
            api::Session::PreMarket => 9,
            api::Session::Regular => 16,
            api::Session::PostMarket => 20,
            api::Session::Overnight => 4,
            _ => 0,
        };
        let session_end_min: u32 = if session == api::Session::PreMarket { 30 } else { 0 };

        let mins_left = if session == api::Session::Overnight && hour >= 20 {
            (24 - hour + 4) * 60 - min
        } else if session_end_hour > hour || (session_end_hour == hour && session_end_min > min) {
            (session_end_hour - hour) * 60 + session_end_min - min
        } else {
            0
        };
        format!("next session in {}h {}m", mins_left / 60, mins_left % 60)
    };

    let (tradable_count, tradable_list) = if !closed {
        match api::fetch_session_limits().await {
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
        }
    } else {
        (None, None)
    };

    let status = if closed { "closed" } else { "open" };

    if json {
        return json_out(&HoursJson {
            status,
            session: session.label(),
            session_hours: session.hours(),
            now: time_str,
            countdown,
            tradable_count,
            tradable: tradable_list,
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
    println!("    Closed:         Fri 8 PM – Sun 8 PM ET");
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
    Ok(())
}

pub async fn list(json: bool, search: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let assets = api::fetch_assets().await.unwrap_or_default();

    let session = api::current_session();
    let tradable_set: std::collections::HashSet<String> = api::fetch_session_limits().await
        .unwrap_or_default()
        .iter()
        .filter(|l| l.is_tradable(session))
        .map(|l| l.symbol.to_uppercase())
        .collect();

    let asset_map: std::collections::HashMap<String, &api::OndoAsset> = assets.iter()
        .map(|a| (a.symbol.to_uppercase(), a))
        .collect();

    let filtered: Vec<_> = match search {
        Some(q) => {
            let q = q.to_lowercase();
            tokens.iter().filter(|t| {
                let sym_match = t.symbol.to_lowercase().contains(&q);
                let name_match = asset_map.get(&t.symbol.to_uppercase())
                    .map(|a| a.asset_name.to_lowercase().contains(&q))
                    .unwrap_or(false);
                let sector_match = asset_map.get(&t.symbol.to_uppercase())
                    .and_then(|a| a.sector())
                    .map(|s| s.to_lowercase().contains(&q))
                    .unwrap_or(false);
                sym_match || name_match || sector_match
            }).collect()
        }
        None => tokens.iter().collect(),
    };

    if json {
        let items: Vec<_> = filtered.iter().map(|t| {
            let asset = asset_map.get(&t.symbol.to_uppercase());
            let name = asset.map(|a| clean_name(&a.asset_name)).unwrap_or_default();
            let kind = asset.and_then(|a| a.instrument_type())
                .unwrap_or_else(|| token_type_from_name(&name))
                .to_lowercase();
            let sector = asset.and_then(|a| a.sector()).map(String::from);
            let tradable = tradable_set.contains(&t.symbol.to_uppercase());
            ListItemJson {
                symbol: t.symbol.to_string(),
                name,
                kind,
                sector,
                tradable,
            }
        }).collect();
        return json_out(&items);
    }

    println!("{} GM tokens{}\n", filtered.len(),
        search.map(|s| format!(" matching '{}'", s)).unwrap_or_default());
    for t in &filtered {
        let asset = asset_map.get(&t.symbol.to_uppercase());
        let name = asset.map(|a| clean_name(&a.asset_name)).unwrap_or_default();
        let sector = asset.and_then(|a| a.sector()).unwrap_or("");
        let mark = if tradable_set.contains(&t.symbol.to_uppercase()) { "✓" } else { "✗" };
        if sector.is_empty() {
            println!("  {} {:<12} {}", mark, t.symbol, name);
        } else {
            println!("  {} {:<12} {:<30} {}", mark, t.symbol, name, sector);
        }
    }
    Ok(())
}
