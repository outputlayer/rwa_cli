use eyre::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::{missing_market_data, parse_market_number, ONDO_API_URL};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OndoAssetTag {
    pub category_slug: String,
    pub tag_label: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OndoAsset {
    pub symbol: String,
    pub asset_name: String,
    #[serde(default)]
    pub tags: Vec<OndoAssetTag>,
    pub primary_market: Option<PrimaryMarket>,
}

impl OndoAsset {
    fn tag(&self, slug: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.category_slug == slug)
            .map(|t| t.tag_label.as_str())
    }

    /// Extract sector from tags (e.g. "Technology", "Healthcare").
    pub fn sector(&self) -> Option<&str> {
        self.tag("sector-industry")
    }

    /// Asset class ("Equities", "Fixed Income", "Commodities", "Crypto-Native
    /// Assets", …) — the primary classification for ETFs without a sector.
    pub fn asset_class(&self) -> Option<&str> {
        self.tag("asset-class")
    }

    /// Region / market exposure ("US", "Asia", "Europe", "Global", …).
    pub fn region(&self) -> Option<&str> {
        self.tag("region-market-exposure")
    }

    /// Every tag label across all categories (sector, asset class, region,
    /// factor/risk profile) — the haystack for tag-based filtering.
    pub fn tag_labels(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(|t| t.tag_label.as_str())
    }

    /// Extract instrument type from tags ("Stock" or "ETF").
    pub fn instrument_type(&self) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.category_slug == "instrument-type")
            .map(|t| t.tag_label.as_str())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryMarket {
    pub price: String,
    #[serde(default)]
    pub price_change_24h: Option<String>,
    #[serde(default)]
    pub price_change_pct_24h: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetsResponse {
    assets: Vec<OndoAsset>,
}

/// Fresh TTL for read-through: asset metadata (names, sectors) is static and
/// prices tolerate a minute of staleness in displays — trading decisions use
/// Jupiter quotes, never these prices.
const ASSETS_CACHE_FRESH: Duration = Duration::from_secs(60);

/// Fetch all assets from the official Ondo API (free, no auth required).
/// Read-through cached at `~/.config/rwa/.cache/assets.json`: a cache younger
/// than 60s serves directly; otherwise fetch live and refresh it. On network
/// failure, falls back to cached data up to 1 hour old. An explicit
/// `RWA_ONDO_API_URL` override (test seam) bypasses the cache entirely.
pub async fn fetch_assets() -> Result<Vec<OndoAsset>> {
    // Read the override once so the cache-bypass decision and the fetch URL
    // can never disagree.
    let env_url = std::env::var("RWA_ONDO_API_URL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let overridden = env_url.is_some();
    let url = env_url.unwrap_or_else(|| ONDO_API_URL.to_string());
    let cache = if overridden {
        None
    } else {
        super::cache_path("assets.json")
    };

    if let Some(assets) = cache
        .as_deref()
        .and_then(|p| super::read_cache(p, ASSETS_CACHE_FRESH))
    {
        return Ok(assets);
    }

    match super::retry_with_backoff(3, || fetch_assets_attempt(&url)).await {
        Ok(assets) => {
            // Update cache (best-effort)
            if let Some(ref path) = cache {
                let _ = super::write_cache(path, &assets);
            }
            Ok(assets)
        }
        Err(live_err) => {
            // Live fetch failed — stale fallback: up to 1 hour old beats a
            // hard failure.
            cache
                .as_deref()
                .and_then(|p| super::read_cache(p, Duration::from_secs(3600)))
                .ok_or(live_err)
        }
    }
}

async fn fetch_assets_attempt(url: &str) -> Result<Vec<OndoAsset>> {
    let wrapper: AssetsResponse = super::get_json(url, "assets").await?;
    Ok(wrapper.assets)
}

/// Find an asset by symbol (case-insensitive, accepts "TSLA" or "TSLAon").
#[must_use]
pub fn find_asset<'a>(symbol: &str, assets: &'a [OndoAsset]) -> Option<&'a OndoAsset> {
    let normalized = symbol.to_uppercase();
    let lookup = if normalized.ends_with("ON") {
        normalized
    } else {
        format!("{normalized}ON")
    };
    assets.iter().find(|a| a.symbol.to_uppercase() == lookup)
}

/// Parse a price string from Ondo market data.
pub fn parse_price(s: &str) -> Result<f64> {
    parse_market_number("price", s, false)
}

/// Parse a 24h change percentage from Ondo market data.
pub fn parse_change_pct(s: &str) -> Result<f64> {
    parse_market_number("price_change_pct_24h", s, true)
}

/// Parse primary market snapshot for a tokenized asset.
pub fn market_snapshot(asset: &OndoAsset) -> Result<(f64, f64)> {
    let pm = asset.primary_market.as_ref().ok_or_else(|| {
        missing_market_data(
            "market_snapshot",
            format!("missing primary market data for {}", asset.symbol),
        )
    })?;
    let price = parse_price(&pm.price)?;
    let pct_24h = match pm.price_change_pct_24h.as_deref() {
        Some(pct) => parse_change_pct(pct)?,
        None => 0.0,
    };
    Ok((price, pct_24h))
}

/// Find an asset by symbol and parse its primary market snapshot.
pub fn market_snapshot_for_symbol(symbol: &str, assets: &[OndoAsset]) -> Result<(f64, f64)> {
    let asset = find_asset(symbol, assets).ok_or_else(|| {
        missing_market_data(
            "market_snapshot",
            format!("missing asset metadata for {symbol}"),
        )
    })?;
    market_snapshot(asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{OndoError, OndoErrorKind};

    #[test]
    fn parse_valid_price() {
        assert!((parse_price("385.75").unwrap() - 385.75).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_integer_price() {
        assert!((parse_price("100").unwrap() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_invalid_price_is_error() {
        assert!(parse_price("N/A").is_err());
        assert!(parse_price("").is_err());
        assert!(parse_price("$100").is_err());
    }

    #[test]
    fn parse_change_pct_allows_negative_values() {
        assert!((parse_change_pct("-5.25").unwrap() + 5.25).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_price_rejects_negative_values() {
        assert!(parse_price("-1").is_err());
    }

    #[test]
    fn parse_market_numbers_reject_non_finite_values() {
        assert!(parse_price("inf").is_err());
        assert!(parse_change_pct("NaN").is_err());
    }

    #[test]
    fn find_by_symbol_with_suffix() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("TSLAon", &assets).is_some());
    }

    #[test]
    fn find_by_symbol_without_suffix() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("TSLA", &assets).is_some());
    }

    #[test]
    fn find_case_insensitive() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("tsla", &assets).is_some());
    }

    #[test]
    fn find_missing_returns_none() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("AAPL", &assets).is_none());
    }

    #[test]
    fn market_snapshot_parses_price_and_change() {
        let asset = OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: Some(PrimaryMarket {
                price: "385.75".into(),
                price_change_24h: Some("4.56".into()),
                price_change_pct_24h: Some("1.20".into()),
            }),
        };

        let (price, pct) = market_snapshot(&asset).unwrap();
        assert!((price - 385.75).abs() < f64::EPSILON);
        assert!((pct - 1.20).abs() < f64::EPSILON);
    }

    #[test]
    fn market_snapshot_for_symbol_fails_loudly_on_invalid_price() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: Some(PrimaryMarket {
                price: "N/A".into(),
                price_change_24h: None,
                price_change_pct_24h: Some("1.20".into()),
            }),
        }];

        let err = market_snapshot_for_symbol("TSLA", &assets).unwrap_err();
        let typed = err.downcast_ref::<OndoError>().expect("typed Ondo error");
        assert_eq!(typed.kind, OndoErrorKind::InvalidData);
        assert_eq!(typed.endpoint, "price");
    }

    #[test]
    fn sector_extraction() {
        let asset = OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![
                OndoAssetTag {
                    category_slug: "sector-industry".into(),
                    tag_label: "Technology".into(),
                },
                OndoAssetTag {
                    category_slug: "instrument-type".into(),
                    tag_label: "Stock".into(),
                },
            ],
            primary_market: None,
        };
        assert_eq!(asset.sector(), Some("Technology"));
        assert_eq!(asset.instrument_type(), Some("Stock"));
    }

    #[test]
    fn missing_tags_return_none() {
        let asset = OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        };
        assert_eq!(asset.sector(), None);
        assert_eq!(asset.instrument_type(), None);
    }
}
