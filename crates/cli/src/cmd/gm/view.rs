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
    /// True when nothing was actually resolved — e.g. every raw term trimmed
    /// to empty and was skipped. `parse_view` uses this to fail closed rather
    /// than silently rendering the whole portfolio in `--view` output shape.
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

    // Every raw term trimmed to empty and was skipped (e.g. `--view ""` from
    // an unset shell variable). The flag was passed, so the caller asked for
    // a slice — silently returning the whole portfolio in --view's output
    // shape would be the misleading outcome; fail closed instead.
    if spec.is_empty() {
        return Err(invalid_view("empty view term — expected a category, tag or ticker"));
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

use super::{GroupJson, PositionJson};

/// Bucket for positions carrying no label in the split category. Deliberately
/// literal: an invented name (e.g. "Equities · US") could not be fed back to
/// `--view` as a term.
pub(super) const UNCLASSIFIED: &str = "Unclassified";

/// Does this position satisfy every facet of the spec? Labels of the same
/// category OR together; different categories AND. Tokens are their own facet.
fn matches(p: &PositionJson, spec: &ViewSpec, catalog_slug_of: &dyn Fn(&str) -> Option<String>) -> bool {
    if !spec.tokens.is_empty() && !spec.tokens.iter().any(|t| t == &p.token) {
        return false;
    }
    // Group the required labels by category, then require one hit per group.
    let mut slugs: Vec<&str> = spec.tags.iter().map(|f| f.slug.as_str()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    for slug in slugs {
        let wanted: Vec<&str> = spec
            .tags
            .iter()
            .filter(|f| f.slug == slug)
            .map(|f| f.label.as_str())
            .collect();
        let hit = p.tags.iter().any(|have| {
            wanted.iter().any(|w| w.eq_ignore_ascii_case(have))
                && catalog_slug_of(have).as_deref() == Some(slug)
        });
        if !hit {
            return false;
        }
    }
    true
}

/// Value-weighted 24h change: back out each position's value 24h ago, exactly
/// as `compute_portfolio` does, so a group's number means the same thing as the
/// portfolio's.
fn weighted_change_pct(positions: &[PositionJson]) -> f64 {
    let value: f64 = positions.iter().map(|p| p.value_usd).sum();
    let prev: f64 = positions
        .iter()
        .map(|p| {
            if p.change_pct_24h.abs() > f64::EPSILON {
                p.value_usd / (1.0 + p.change_pct_24h / 100.0)
            } else {
                p.value_usd
            }
        })
        .sum();
    if prev.abs() > f64::EPSILON {
        (value / prev - 1.0) * 100.0
    } else {
        0.0
    }
}

fn build_group(name: Option<String>, mut positions: Vec<PositionJson>, gm_total: f64) -> GroupJson {
    let value_usd: f64 = positions.iter().map(|p| p.value_usd).sum();
    for p in &mut positions {
        p.group_alloc_pct = Some(if value_usd.abs() > f64::EPSILON {
            p.value_usd / value_usd * 100.0
        } else {
            0.0
        });
    }
    GroupJson {
        group: name,
        value_usd,
        gm_alloc_pct: if gm_total.abs() > f64::EPSILON { value_usd / gm_total * 100.0 } else { 0.0 },
        change_24h_pct: weighted_change_pct(&positions),
        positions,
    }
}

/// Labels this position carries in one category. Sector/class/region/type live
/// in their own fields; factor labels are whatever remains in `tags`.
fn labels_of(p: &PositionJson, cat: Category) -> Vec<String> {
    match cat {
        Category::Sector => p.sector.clone().into_iter().collect(),
        Category::Class => p.asset_class.clone().into_iter().collect(),
        Category::Region => p.region.clone().into_iter().collect(),
        Category::Type => p.kind.clone().into_iter().collect(),
        Category::Factor => {
            let single: Vec<&str> = [&p.sector, &p.asset_class, &p.region, &p.kind]
                .into_iter()
                .filter_map(|o| o.as_deref())
                .collect();
            p.tags
                .iter()
                .filter(|t| !single.iter().any(|s| s.eq_ignore_ascii_case(t)))
                .cloned()
                .collect()
        }
    }
}

/// Apply a parsed view: filter, then split. Returns the surviving positions,
/// the groups, and whether groups can overlap (i.e. whether summing them is
/// meaningless).
pub(super) fn apply_view(
    positions: Vec<PositionJson>,
    spec: &ViewSpec,
) -> (Vec<PositionJson>, Vec<GroupJson>, bool) {
    let gm_total: f64 = positions.iter().map(|p| p.value_usd).sum();

    // A position's tag → category lookup, derived from the positions themselves
    // (they already carry their labels; `slug_of` maps label → slug via the
    // spec's own filters plus the split category).
    let known: Vec<TagFilter> = spec.tags.clone();
    let slug_of = move |label: &str| -> Option<String> {
        known
            .iter()
            .find(|f| f.label.eq_ignore_ascii_case(label))
            .map(|f| f.slug.clone())
    };

    let filtered: Vec<PositionJson> = positions
        .into_iter()
        .filter(|p| matches(p, spec, &slug_of))
        .collect();

    if filtered.is_empty() {
        return (filtered, Vec::new(), false);
    }

    let Some(cat) = spec.split else {
        let group = build_group(None, filtered.clone(), gm_total);
        return (filtered, vec![group], false);
    };

    // Bucket by every label the position carries in the split category.
    let mut named: Vec<(String, Vec<PositionJson>)> = Vec::new();
    let mut unclassified: Vec<PositionJson> = Vec::new();
    let mut overlapping = false;

    for p in &filtered {
        let labels = labels_of(p, cat);
        if labels.is_empty() {
            unclassified.push(p.clone());
            continue;
        }
        if labels.len() > 1 {
            overlapping = true;
        }
        for label in labels {
            match named.iter_mut().find(|(n, _)| n == &label) {
                Some((_, bucket)) => bucket.push(p.clone()),
                None => named.push((label, vec![p.clone()])),
            }
        }
    }

    let mut groups: Vec<GroupJson> = named
        .into_iter()
        .map(|(name, ps)| build_group(Some(name), ps, gm_total))
        .collect();

    // Deterministic order: value desc, ties broken by name so output and tests
    // never depend on insertion order.
    groups.sort_by(|a, b| {
        b.value_usd
            .partial_cmp(&a.value_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.group.cmp(&b.group))
    });

    if !unclassified.is_empty() {
        groups.push(build_group(Some(UNCLASSIFIED.to_string()), unclassified, gm_total));
    }

    (filtered, groups, overlapping)
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

    /// breaks if: a term that trims to empty (e.g. `--view ""` from an unset
    /// shell variable) is silently skipped and parse_view returns Ok with an
    /// empty spec — apply_view would then treat that as "no filter" and
    /// render the WHOLE portfolio as one anonymous group under --view's
    /// output shape, which looks like a slice but isn't one. Also pins that
    /// a legitimate term alongside blanks is still accepted (the guard must
    /// fire only when NOTHING resolved, not whenever any term is blank).
    #[test]
    fn all_blank_terms_fail_closed_but_a_real_term_among_blanks_still_works() {
        let err = spec(&[""]).unwrap_err().to_string();
        assert!(err.contains("invalid_view"), "must be the typed kind: {err}");
        assert!(err.contains("empty view term"), "message must name the problem: {err}");

        let err = spec(&["  "]).unwrap_err().to_string();
        assert!(err.contains("invalid_view"), "whitespace-only must fail too: {err}");

        let s = spec(&["", "sector"]).unwrap();
        assert_eq!(s.split, Some(Category::Sector), "a real term among blanks must still parse");
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

    fn pos(token: &str, value: f64, pct24: f64, tags: &[&str]) -> PositionJson {
        PositionJson {
            token: token.to_string(),
            balance: 1.0,
            price: value,
            value_usd: value,
            gm_alloc_pct: 0.0, // filled by the caller in real code; irrelevant here
            change_pct_24h: pct24,
            shares_per_token: None,
            sector: None,
            asset_class: None,
            region: None,
            kind: None,
            tags: tags.iter().map(|s| (*s).to_string()).collect(),
            group_alloc_pct: None,
        }
    }

    /// Portfolio fixture: two overlapping factor tags, one position with no
    /// factor tag at all (must land in Unclassified).
    fn portfolio() -> Vec<PositionJson> {
        vec![
            pos("PFEon", 976.11, -0.96, &["Dividend", "Large Cap"]),
            pos("LLYon", 721.14, -4.47, &["Dividend", "Growth", "Large Cap"]),
            pos("MRNAon", 177.90, 6.79, &["Growth"]),
            pos("AALon", 100.00, 1.00, &[]),
        ]
    }

    fn split_spec(cat: Category) -> ViewSpec {
        ViewSpec { split: Some(cat), ..ViewSpec::default() }
    }

    /// breaks if: a position lands in more than one group for a single-valued
    /// category, or Unclassified is dropped — either way the sum stops
    /// matching. Also breaks if the `Category::Sector` arm of `labels_of`
    /// reads the wrong field: `sector` is set explicitly (not via `tags`,
    /// which `pos()` never copies into the single-valued fields), so a
    /// mislabeled arm (e.g. reading `kind` instead) yields no "Healthcare"
    /// group at all and both positions fall into one Unclassified bucket —
    /// caught by the explicit group count/name/value checks below.
    #[test]
    fn non_overlapping_split_sums_exactly_to_the_total() {
        let positions = vec![
            PositionJson { sector: Some("Healthcare".to_string()), ..pos("PFEon", 976.11, -0.96, &[]) },
            pos("SPYon", 500.00, 0.50, &[]),
        ];
        let total: f64 = positions.iter().map(|p| p.value_usd).sum();
        let mut spec = split_spec(Category::Sector);
        spec.split = Some(Category::Sector);

        let (_, groups, overlapping) = apply_view(positions, &spec);
        assert!(!overlapping, "single-valued category must not report overlap");
        let sum: f64 = groups.iter().map(|g| g.value_usd).sum();
        assert!((sum - total).abs() < 1e-6, "groups {sum} must sum to total {total}");
        assert_eq!(groups.last().unwrap().group.as_deref(), Some(UNCLASSIFIED),
                   "untagged positions go last");
        assert_eq!(groups.len(), 2, "expected a Healthcare group plus Unclassified, not a single catch-all");
        assert_eq!(groups[0].group.as_deref(), Some("Healthcare"), "Sector split must read p.sector");
        assert!((groups[0].value_usd - 976.11).abs() < 1e-6);
    }

    /// breaks if: any two of the Sector/Class/Region/Type arms in `labels_of`
    /// are swapped (they are structurally identical field reads — an easy
    /// copy-paste error). Each position carries a DIFFERENT label per field,
    /// so reading the wrong field changes which group it lands in and the
    /// group names below stop matching.
    #[test]
    fn split_by_class_region_and_type_each_reads_its_own_field() {
        let one = PositionJson {
            asset_class: Some("Equities".to_string()),
            region: Some("US".to_string()),
            kind: Some("Stock".to_string()),
            ..pos("One", 100.0, 0.0, &[])
        };
        let two = PositionJson {
            asset_class: Some("Fixed Income".to_string()),
            region: Some("Asia".to_string()),
            kind: Some("ETF".to_string()),
            ..pos("Two", 200.0, 0.0, &[])
        };

        let (_, class_groups, _) = apply_view(vec![one.clone(), two.clone()], &split_spec(Category::Class));
        let class_names: Vec<&str> = class_groups.iter().map(|g| g.group.as_deref().unwrap()).collect();
        assert_eq!(class_names, vec!["Fixed Income", "Equities"], "Class split must read p.asset_class");

        let (_, region_groups, _) = apply_view(vec![one.clone(), two.clone()], &split_spec(Category::Region));
        let region_names: Vec<&str> = region_groups.iter().map(|g| g.group.as_deref().unwrap()).collect();
        assert_eq!(region_names, vec!["Asia", "US"], "Region split must read p.region");

        let (_, type_groups, _) = apply_view(vec![one, two], &split_spec(Category::Type));
        let type_names: Vec<&str> = type_groups.iter().map(|g| g.group.as_deref().unwrap()).collect();
        assert_eq!(type_names, vec!["ETF", "Stock"], "Type split must read p.kind");
    }

    /// breaks if: overlap is not detected — an agent summing groups would then
    /// silently read 200%+ of the portfolio as truth.
    #[test]
    fn overlapping_split_flags_itself_and_exceeds_the_total() {
        let positions = portfolio();
        let total: f64 = positions.iter().map(|p| p.value_usd).sum();
        let (_, groups, overlapping) = apply_view(positions, &split_spec(Category::Factor));

        assert!(overlapping, "multi-valued factor tags must set the flag");
        let sum: f64 = groups.iter().map(|g| g.value_usd).sum();
        assert!(sum > total, "overlapping groups must exceed total: {sum} vs {total}");
    }

    /// breaks if: group ordering depends on HashMap iteration — flaky output
    /// and flaky tests.
    #[test]
    fn groups_are_ordered_by_value_then_name_with_unclassified_last() {
        let positions = vec![
            pos("Bon", 100.0, 0.0, &["Beta"]),
            pos("Aon", 100.0, 0.0, &["Alpha"]),
            pos("Con", 300.0, 0.0, &["Gamma"]),
            pos("Don", 50.0, 0.0, &[]),
        ];
        let (_, groups, _) = apply_view(positions, &split_spec(Category::Factor));
        let names: Vec<&str> = groups.iter().map(|g| g.group.as_deref().unwrap_or("?")).collect();
        assert_eq!(names, vec!["Gamma", "Alpha", "Beta", UNCLASSIFIED],
                   "value desc, ties by name, Unclassified pinned last");
    }

    /// breaks if: filtering recomputes gm_alloc_pct against the filtered subset
    /// — the field would then mean different things with and without --view.
    #[test]
    fn filtering_does_not_touch_gm_alloc_pct() {
        let mut positions = portfolio();
        positions[0].gm_alloc_pct = 11.28;
        let spec = ViewSpec {
            tags: vec![TagFilter { slug: "type-factor-risk-profile".into(), label: "Dividend".into() }],
            ..ViewSpec::default()
        };
        let (filtered, _, _) = apply_view(positions, &spec);
        assert_eq!(filtered.len(), 2, "only Dividend positions survive");
        assert_eq!(filtered[0].gm_alloc_pct, 11.28, "share of the FULL portfolio is preserved");
    }

    /// breaks if: same-category labels are ANDed (would return nothing) or
    /// cross-category labels are ORed (would return too much).
    #[test]
    fn facets_or_within_a_category_and_and_across_categories() {
        let positions = vec![
            pos("PFEon", 100.0, 0.0, &["Dividend", "Healthcare"]),
            pos("MRNAon", 100.0, 0.0, &["Growth", "Healthcare"]),
            pos("NVDAon", 100.0, 0.0, &["Growth", "Technology"]),
        ];
        let factor = |l: &str| TagFilter { slug: "type-factor-risk-profile".into(), label: l.into() };
        let sector = |l: &str| TagFilter { slug: "sector-industry".into(), label: l.into() };

        let or_spec = ViewSpec { tags: vec![factor("Dividend"), factor("Growth")], ..ViewSpec::default() };
        let (or_hit, _, _) = apply_view(positions.clone(), &or_spec);
        assert_eq!(or_hit.len(), 3, "same-category labels must OR");

        let and_spec = ViewSpec { tags: vec![factor("Growth"), sector("Healthcare")], ..ViewSpec::default() };
        let (and_hit, _, _) = apply_view(positions, &and_spec);
        assert_eq!(and_hit.len(), 1, "cross-category labels must AND");
        assert_eq!(and_hit[0].token, "MRNAon");
    }

    /// breaks if: group_alloc_pct is computed against the portfolio instead of
    /// the group, or the group's 24h is a plain average instead of value-weighted.
    ///
    /// The third, differently-tagged position is load-bearing: it makes
    /// gm_total (1500) diverge from the Growth group's own value_usd (1000),
    /// so a group_alloc_pct computed against gm_total (60%/6.67%) is
    /// numerically distinguishable from one computed against the group
    /// (90%/10%) — without it, a single all-covering group makes the two
    /// divisors coincide and the mutation below is invisible.
    #[test]
    fn group_percentages_are_relative_to_the_group_and_24h_is_value_weighted() {
        let positions = vec![
            pos("Big", 900.0, 10.0, &["Growth"]),
            pos("Small", 100.0, -10.0, &["Growth"]),
            pos("Third", 500.0, 0.0, &["Value"]),
        ];
        let (_, groups, _) = apply_view(positions, &split_spec(Category::Factor));
        let g = groups.iter().find(|g| g.group.as_deref() == Some("Growth")).unwrap();

        let shares: Vec<f64> = g.positions.iter().map(|p| p.group_alloc_pct.unwrap()).collect();
        assert!((shares[0] - 90.0).abs() < 1e-6, "group share, not portfolio share: {shares:?}");
        assert!((shares.iter().sum::<f64>() - 100.0).abs() < 1e-6);

        // Value-weighted: 900 grew 10%, 100 fell 10% → prev = 818.18 + 111.11
        // → 1000/929.29 - 1 = +7.61%, NOT the naive average of 0%.
        assert!((g.change_24h_pct - 7.61).abs() < 0.05, "got {}", g.change_24h_pct);
    }

    /// breaks if: an empty result is turned into an error or a phantom group.
    #[test]
    fn a_filter_matching_nothing_yields_no_groups_and_no_error() {
        let spec = ViewSpec {
            tags: vec![TagFilter { slug: "sector-industry".into(), label: "Energy".into() }],
            ..ViewSpec::default()
        };
        let (filtered, groups, overlapping) = apply_view(portfolio(), &spec);
        assert!(filtered.is_empty());
        assert!(groups.is_empty(), "no positions means no groups, not an empty-named group");
        assert!(!overlapping);
    }

    /// breaks if: filter-only mode stops producing the single anonymous group
    /// agents rely on for a branch-free parser.
    #[test]
    fn filter_without_split_yields_exactly_one_anonymous_group() {
        let spec = ViewSpec {
            tokens: vec!["MRNAon".to_string()],
            ..ViewSpec::default()
        };
        let (_, groups, _) = apply_view(portfolio(), &spec);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].group.is_none(), "no split means group: null");
        assert_eq!(groups[0].positions.len(), 1);
    }
}
