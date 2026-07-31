//! `--view` term parsing: one polymorphic argument resolved against three
//! non-overlapping dictionaries — five reserved category names (ours), catalog
//! tickers, and Ondo tag labels. A live catalog snapshot (2026-07-31, 444
//! assets) has zero overlap between the three; `tag:` / `token:` / `sector:`
//! prefixes exist as an escape hatch if that ever changes.

use eyre::Result;
use rwa_ondo::api::{self, OndoAsset};
use rwa_ondo::symbol_resolve::closest_match;
use rwa_ondo::usecases::gm::{GmTradeError, GmTradeErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Category {
    Sector,
    Region,
    Class,
    Type,
    Factor,
}

impl Category {
    pub(super) const ALL: [Category; 5] = [
        Self::Sector,
        Self::Region,
        Self::Class,
        Self::Type,
        Self::Factor,
    ];

    /// The user-facing word. Deliberately ours, not Ondo's slug: `class` reads
    /// better than `asset-class` and insulates the flag from catalog renames.
    pub(super) fn term(self) -> &'static str {
        match self {
            Self::Sector => "sector",
            Self::Region => "region",
            Self::Class => "class",
            Self::Type => "type",
            Self::Factor => "factor",
        }
    }

    pub(super) fn slug(self) -> &'static str {
        match self {
            Self::Sector => "sector-industry",
            Self::Region => "region-market-exposure",
            Self::Class => "asset-class",
            Self::Type => "instrument-type",
            Self::Factor => "type-factor-risk-profile",
        }
    }

    fn from_term(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.term().eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TagFilter {
    /// Ondo `categorySlug` the label belongs to — drives OR-within-category,
    /// AND-across-categories faceting.
    pub(super) slug: String,
    pub(super) label: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ViewSpec {
    /// Terms exactly as the user typed them — echoed back in output so the
    /// polymorphic flag never has to be guessed at.
    pub(super) terms: Vec<String>,
    pub(super) tags: Vec<TagFilter>,
    pub(super) tokens: Vec<String>,
    pub(super) split: Option<Category>,
}

impl ViewSpec {
    pub(super) fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.tokens.is_empty() && self.split.is_none()
    }
}

fn invalid_view(detail: impl Into<String>) -> eyre::Report {
    GmTradeError::new(GmTradeErrorKind::InvalidView, detail).into()
}

/// Every distinct tag label in the catalog, paired with its category slug.
fn catalog_labels(assets: &[OndoAsset]) -> Vec<TagFilter> {
    let mut out: Vec<TagFilter> = Vec::new();
    for a in assets {
        for t in &a.tags {
            let f = TagFilter {
                slug: t.category_slug.clone(),
                label: t.tag_label.clone(),
            };
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

fn find_label(needle: &str, labels: &[TagFilter], within: Option<Category>) -> Option<TagFilter> {
    labels
        .iter()
        .find(|f| {
            f.label.eq_ignore_ascii_case(needle)
                && within.is_none_or(|c| f.slug == c.slug())
        })
        .cloned()
}

/// Resolve `--view` terms against the catalog. Order is load-bearing:
/// reserved category names win over labels and tickers, so a future Ondo
/// label named "sector" cannot change what the flag does.
pub(super) fn parse_view(terms: &[String], assets: &[OndoAsset]) -> Result<ViewSpec> {
    let labels = catalog_labels(assets);
    let mut spec = ViewSpec::default();

    for raw in terms {
        let term = raw.trim();
        if term.is_empty() {
            continue;
        }
        spec.terms.push(term.to_string());

        // Explicit prefixes bypass auto-detection entirely.
        if let Some(rest) = term.strip_prefix("token:") {
            push_token(&mut spec, rest, assets)?;
            continue;
        }
        if let Some(rest) = term.strip_prefix("tag:") {
            let f = find_label(rest, &labels, None)
                .ok_or_else(|| invalid_view(unknown_term_msg(rest, &labels, assets)))?;
            push_tag(&mut spec, f);
            continue;
        }
        if let Some((prefix, rest)) = term.split_once(':')
            && let Some(cat) = Category::from_term(prefix)
        {
            let f = find_label(rest, &labels, Some(cat))
                .ok_or_else(|| invalid_view(unknown_term_msg(rest, &labels, assets)))?;
            push_tag(&mut spec, f);
            continue;
        }

        // 1. reserved category name → split
        if let Some(cat) = Category::from_term(term) {
            match spec.split {
                Some(existing) if existing != cat => {
                    return Err(invalid_view(format!(
                        "cannot split by both `{}` and `{}` — one dimension only",
                        existing.term(),
                        cat.term()
                    )));
                }
                _ => spec.split = Some(cat),
            }
            continue;
        }

        // 2. catalog ticker → token filter
        if api::find_asset(term, assets).is_some() {
            push_token(&mut spec, term, assets)?;
            continue;
        }

        // 3. catalog label → tag filter
        if let Some(f) = find_label(term, &labels, None) {
            push_tag(&mut spec, f);
            continue;
        }

        return Err(invalid_view(unknown_term_msg(term, &labels, assets)));
    }

    Ok(spec)
}

fn push_token(spec: &mut ViewSpec, raw: &str, assets: &[OndoAsset]) -> Result<()> {
    let asset = api::find_asset(raw, assets)
        .ok_or_else(|| invalid_view(format!("unknown view term '{raw}': no such GM token")))?;
    if !spec.tokens.contains(&asset.symbol) {
        spec.tokens.push(asset.symbol.clone());
    }
    Ok(())
}

fn push_tag(spec: &mut ViewSpec, f: TagFilter) {
    if !spec.tags.contains(&f) {
        spec.tags.push(f);
    }
}

fn unknown_term_msg(term: &str, labels: &[TagFilter], assets: &[OndoAsset]) -> String {
    let cats = Category::ALL.iter().map(|c| c.term()).collect::<Vec<_>>().join(", ");
    let candidates: Vec<&str> = labels
        .iter()
        .map(|f| f.label.as_str())
        .chain(assets.iter().map(|a| a.symbol.as_str()))
        .collect();
    let hint = closest_match(term, candidates)
        .map(|s| format!(" Did you mean '{s}'?"))
        .unwrap_or_default();
    format!("unknown view term '{term}'.{hint} Categories: {cats}. Tags and tickers come from the Ondo catalog.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rwa_ondo::api::{OndoAsset, OndoAssetTag};

    fn asset(symbol: &str, tags: &[(&str, &str)]) -> OndoAsset {
        OndoAsset {
            symbol: symbol.to_string(),
            asset_name: format!("{symbol} Inc"),
            tags: tags
                .iter()
                .map(|(slug, label)| OndoAssetTag {
                    category_slug: (*slug).to_string(),
                    tag_label: (*label).to_string(),
                })
                .collect(),
            primary_market: None,
            is_trading_paused: false,
            is_offhours_tradable: false,
        }
    }

    /// Catalog fixture mirroring the live shape: sectors are single-valued,
    /// factor tags are multi-valued, some assets carry no region.
    fn catalog() -> Vec<OndoAsset> {
        vec![
            asset("PFEon", &[
                ("sector-industry", "Healthcare"),
                ("asset-class", "Equities"),
                ("region-market-exposure", "US"),
                ("instrument-type", "Stock"),
                ("type-factor-risk-profile", "Dividend"),
                ("type-factor-risk-profile", "Large Cap"),
            ]),
            asset("MRNAon", &[
                ("sector-industry", "Healthcare"),
                ("asset-class", "Equities"),
                ("instrument-type", "Stock"),
                ("type-factor-risk-profile", "Growth"),
            ]),
            asset("SPYon", &[
                ("asset-class", "Equities"),
                ("region-market-exposure", "US"),
                ("instrument-type", "ETF"),
                ("type-factor-risk-profile", "Index"),
            ]),
        ]
    }

    fn spec(terms: &[&str]) -> eyre::Result<ViewSpec> {
        let owned: Vec<String> = terms.iter().map(|s| (*s).to_string()).collect();
        parse_view(&owned, &catalog())
    }

    /// breaks if: category names stop being hardcoded, or the split slot is
    /// not populated from a bare category term.
    #[test]
    fn bare_category_term_becomes_a_split() {
        let s = spec(&["sector"]).unwrap();
        assert_eq!(s.split, Some(Category::Sector));
        assert!(s.tags.is_empty() && s.tokens.is_empty(), "a category must not filter");
    }

    /// breaks if: ticker normalization is dropped — `--view MRNA` must reach
    /// the position stored as `MRNAon`.
    #[test]
    fn ticker_term_normalizes_to_the_canonical_symbol() {
        assert_eq!(spec(&["MRNA"]).unwrap().tokens, vec!["MRNAon".to_string()]);
        assert_eq!(spec(&["mrnaon"]).unwrap().tokens, vec!["MRNAon".to_string()]);
    }

    /// breaks if: label matching becomes case-sensitive, or the tag's category
    /// is not recorded (facet OR/AND logic needs the slug).
    #[test]
    fn label_term_becomes_a_tag_filter_with_its_category() {
        let s = spec(&["dividend"]).unwrap();
        assert_eq!(s.tags.len(), 1);
        assert_eq!(s.tags[0].label, "Dividend", "canonical casing from the catalog");
        assert_eq!(s.tags[0].slug, "type-factor-risk-profile");
    }

    /// breaks if: the hardcoded category dictionary loses priority over Ondo
    /// labels. Guards the rule, not today's lucky catalog contents.
    #[test]
    fn category_names_win_over_a_colliding_ondo_label() {
        let mut colliding = catalog();
        colliding.push(asset("XYZon", &[("type-factor-risk-profile", "sector")]));
        let terms = vec!["sector".to_string()];
        let s = parse_view(&terms, &colliding).unwrap();
        assert_eq!(s.split, Some(Category::Sector), "reserved word must stay a category");
        assert!(s.tags.is_empty());

        let explicit = vec!["tag:sector".to_string()];
        let s = parse_view(&explicit, &colliding).unwrap();
        assert!(s.split.is_none(), "tag: prefix must escape to the label");
        assert_eq!(s.tags[0].label, "sector");
    }

    /// breaks if: explicit prefixes diverge from auto-detection.
    #[test]
    fn explicit_prefix_matches_auto_detection() {
        assert_eq!(spec(&["sector:Healthcare"]).unwrap().tags, spec(&["Healthcare"]).unwrap().tags);
        assert_eq!(spec(&["token:MRNA"]).unwrap().tokens, spec(&["MRNA"]).unwrap().tokens);
    }

    /// breaks if: duplicates accumulate (would double-apply a filter and skew
    /// any future counting).
    #[test]
    fn duplicate_terms_are_idempotent() {
        assert_eq!(spec(&["Dividend", "Dividend"]).unwrap().tags.len(), 1);
        assert_eq!(spec(&["MRNA", "mrnaon"]).unwrap().tokens.len(), 1);
    }

    /// breaks if: two categories are silently accepted — the output can only
    /// be split along one dimension.
    #[test]
    fn two_categories_are_rejected() {
        let err = spec(&["sector", "region"]).unwrap_err().to_string();
        assert!(err.contains("invalid_view"), "must be the typed kind: {err}");
        assert!(err.contains("one dimension"), "message must say why: {err}");
    }

    /// breaks if: unknown terms are swallowed as empty filters instead of
    /// failing loudly, or the did-you-mean suggestion is lost.
    #[test]
    fn unknown_term_fails_with_suggestion_and_typed_kind() {
        let err = spec(&["Enrgy"]).unwrap_err().to_string();
        assert!(err.contains("invalid_view"), "typed kind: {err}");
        assert!(err.contains("Enrgy"), "must echo the offending term: {err}");

        let far = spec(&["biopharma"]).unwrap_err().to_string();
        assert!(far.contains("sector, region, class, type, factor"), "must list categories: {far}");
    }

    /// breaks if: `sector`/`class` term-to-slug mapping is inverted or dropped.
    #[test]
    fn category_terms_map_to_ondo_slugs() {
        assert_eq!(Category::Sector.slug(), "sector-industry");
        assert_eq!(Category::Class.slug(), "asset-class");
        assert_eq!(Category::Type.slug(), "instrument-type");
        assert_eq!(Category::Region.slug(), "region-market-exposure");
        assert_eq!(Category::Factor.slug(), "type-factor-risk-profile");
    }
}
