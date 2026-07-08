//! Per-prefix recency-decay map + the pure post-fusion recency boost.
//!
//! Mirrors the TS `src/core/search/recency-decay.ts` config module and the
//! `applyRecencyBoost` stage in `src/core/search/hybrid.ts:185`. This is a pure
//! module: no I/O, no clock reads, no engine access. The engine layer resolves
//! the effective map (defaults + zbrain.yml + env + caller overrides), reads
//! `SystemTime::now()`, parses effective-date strings to unix-ms, and then calls
//! [`apply_recency_boost`] — keeping the engine a pure scoring machine and this
//! module fully deterministic under test (`now_ms` is injected, never read).
//!
//! Per-prefix interpretation (mirrors TS doc):
//!   - `halflife_days == 0`  → evergreen, no decay (recency component = 0)
//!   - `halflife_days > 0`   → hyperbolic decay:
//!     `coefficient * halflife / (halflife + days_old)`
//!   - at `days_old == 0`:        recency component = `coefficient` (max boost)
//!   - at `days_old == halflife`: recency component = `coefficient / 2`
//!
//! Override priority (later wins), matching TS `resolveRecencyDecayMap`:
//!   1. [`default_recency_decay`] (this file)
//!   2. `zbrain.yml` `recency:` section
//!   3. `ZBRAIN_RECENCY_DECAY` env var (`prefix:halflifeDays:coefficient,...`)
//!   4. per-call caller overrides

use std::collections::HashMap;

/// Per-prefix decay configuration. Mirrors TS `RecencyDecayConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecencyDecayConfig {
    /// Days at which the recency component is halved. `0` = evergreen (no decay).
    pub halflife_days: f64,
    /// Max recency boost contribution at `days_old == 0`. Must be `>= 0`.
    pub coefficient: f64,
}

/// Prefix → decay config. Mirrors TS `RecencyDecayMap` (`Record<string, …>`).
pub type RecencyDecayMap = HashMap<String, RecencyDecayConfig>;

/// Recency boost strength. Mirrors the TS `'on' | 'strong'` union. `'on'`
/// multiplies the coefficient by 1.0; `'strong'` by 1.5 (more aggressive tilt).
///
/// Resolving which strength the active search mode wants is NOT migrated yet
/// (the search-mode system is unported), so the engine hardcodes `On` for now —
/// registered in docs/plans/KNOWN-GAPS.md (G13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecencyStrength {
    On,
    Strong,
}

impl RecencyStrength {
    #[must_use]
    pub fn multiplier(self) -> f64 {
        match self {
            RecencyStrength::On => 1.0,
            RecencyStrength::Strong => 1.5,
        }
    }
}

/// Fallback applied to slugs that match no prefix. Mirrors TS `DEFAULT_FALLBACK`.
pub const DEFAULT_FALLBACK: RecencyDecayConfig = RecencyDecayConfig {
    halflife_days: 90.0,
    coefficient: 0.5,
};

/// Default per-prefix decay map. Mirrors TS `DEFAULT_RECENCY_DECAY` exactly
/// (generic prefixes only — fork-specific names live in user zbrain.yml, never
/// in shipped defaults, per the privacy rule cited in the TS module).
#[must_use]
pub fn default_recency_decay() -> RecencyDecayMap {
    // (prefix, halflife_days, coefficient)
    const ENTRIES: &[(&str, f64, f64)] = &[
        ("concepts/", 0.0, 0.0),
        ("originals/", 180.0, 0.5),
        ("writing/", 365.0, 0.4),
        ("daily/", 14.0, 1.5),
        ("meetings/", 60.0, 1.0),
        ("chat/", 7.0, 1.0),
        ("media/x/", 7.0, 1.5),
        ("media/articles/", 90.0, 0.5),
        ("people/", 365.0, 0.3),
        ("companies/", 365.0, 0.3),
        ("deals/", 180.0, 0.5),
    ];
    ENTRIES
        .iter()
        .map(|&(p, hl, coef)| {
            (
                p.to_string(),
                RecencyDecayConfig {
                    halflife_days: hl,
                    coefficient: coef,
                },
            )
        })
        .collect()
}

/// Source of a decay-map parse failure. Mirrors TS `RecencyDecayParseError.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecencyDecaySource {
    Env,
    Yaml,
}

/// Parse error from the env / yaml parsers. Mirrors TS `RecencyDecayParseError`.
/// The parsers refuse (rather than silently skip) malformed entries so
/// misconfigurations surface at startup instead of silently degrading rankings
/// (TS codex finding M-CX-3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecencyDecayParseError {
    pub message: String,
    pub source: RecencyDecaySource,
}

impl std::fmt::Display for RecencyDecayParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RecencyDecayParseError {}

/// Parse the `ZBRAIN_RECENCY_DECAY` env var.
/// Format: comma-separated `prefix:halflifeDays:coefficient` triples, e.g.
/// `"daily/:7:2.0,concepts/:0:0,custom/:30:1.0"`.
///
/// Fails LOUD on any malformed entry (mirrors TS `parseRecencyDecayEnv`). The
/// prefix may contain `/` but NOT `:`; the two numeric fields are split from
/// the right so a `/`-heavy prefix stays intact.
pub fn parse_recency_decay_env(
    env: Option<&str>,
) -> Result<RecencyDecayMap, RecencyDecayParseError> {
    let Some(env) = env else {
        return Ok(HashMap::new());
    };
    let bad = |triple: &str| RecencyDecayParseError {
        message: format!(
            "Invalid ZBRAIN_RECENCY_DECAY entry \"{triple}\": expected prefix:halflife:coefficient"
        ),
        source: RecencyDecaySource::Env,
    };
    let mut out = HashMap::new();
    for triple in env.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        // Split on the LAST and SECOND-LAST ':' so the prefix may contain '/'.
        let last_idx = triple.rfind(':').filter(|&i| i > 0).ok_or_else(|| bad(triple))?;
        let before_last = &triple[..last_idx];
        let middle_idx = before_last
            .rfind(':')
            .filter(|&i| i > 0)
            .ok_or_else(|| bad(triple))?;
        let prefix = triple[..middle_idx].trim();
        let halflife_raw = triple[middle_idx + 1..last_idx].trim();
        let coefficient_raw = triple[last_idx + 1..].trim();
        if prefix.is_empty() {
            return Err(RecencyDecayParseError {
                message: format!("Empty prefix in ZBRAIN_RECENCY_DECAY entry \"{triple}\""),
                source: RecencyDecaySource::Env,
            });
        }
        let halflife = halflife_raw
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| RecencyDecayParseError {
                message: format!(
                    "Invalid halflifeDays \"{halflife_raw}\" in ZBRAIN_RECENCY_DECAY (must be number >= 0; 0 = evergreen)"
                ),
                source: RecencyDecaySource::Env,
            })?;
        let coefficient = coefficient_raw
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| RecencyDecayParseError {
                message: format!(
                    "Invalid coefficient \"{coefficient_raw}\" in ZBRAIN_RECENCY_DECAY (must be number >= 0)"
                ),
                source: RecencyDecaySource::Env,
            })?;
        out.insert(
            prefix.to_string(),
            RecencyDecayConfig {
                halflife_days: halflife,
                coefficient,
            },
        );
    }
    Ok(out)
}

/// Parse a `recency:` section from an already-parsed `zbrain.yml` value.
/// Shape mirrors TS `parseRecencyDecayYaml`:
///
/// ```yaml
/// recency:
///   daily/: { halflifeDays: 14, coefficient: 1.5 }
///   concepts/: { halflifeDays: 0, coefficient: 0 }
/// ```
///
/// `null` / missing `recency:` returns an empty map. A present-but-non-map
/// `recency:` fails LOUD, as does an entry that is not a map or whose
/// `halflifeDays` / `coefficient` are not finite numbers `>= 0`.
pub fn parse_recency_decay_yaml(
    parsed: Option<&serde_yaml::Value>,
) -> Result<RecencyDecayMap, RecencyDecayParseError> {
    let err = |msg: String| RecencyDecayParseError {
        message: msg,
        source: RecencyDecaySource::Yaml,
    };
    let Some(parsed) = parsed else {
        return Ok(HashMap::new());
    };
    let Some(top) = parsed.as_mapping() else {
        return Ok(HashMap::new());
    };
    let Some(recency) = top.get(serde_yaml::Value::String("recency".to_string())) else {
        return Ok(HashMap::new());
    };
    if recency.is_null() {
        return Ok(HashMap::new());
    }
    let Some(recency_map) = recency.as_mapping() else {
        return Err(err("zbrain.yml recency: must be a map".to_string()));
    };
    let mut out = HashMap::new();
    for (k, raw) in recency_map {
        let prefix = k.as_str().unwrap_or_default().to_string();
        let Some(cfg) = raw.as_mapping() else {
            return Err(err(format!(
                "zbrain.yml recency.\"{prefix}\" must be an object with halflifeDays + coefficient"
            )));
        };
        let read_num = |field: &str| -> Option<f64> {
            cfg.get(serde_yaml::Value::String(field.to_string()))
                .and_then(serde_yaml::Value::as_f64)
        };
        let halflife = read_num("halflifeDays")
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| {
                err(format!(
                    "zbrain.yml recency.\"{prefix}\".halflifeDays invalid (must be number >= 0)"
                ))
            })?;
        let coefficient = read_num("coefficient")
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| {
                err(format!(
                    "zbrain.yml recency.\"{prefix}\".coefficient invalid (must be number >= 0)"
                ))
            })?;
        out.insert(
            prefix,
            RecencyDecayConfig {
                halflife_days: halflife,
                coefficient,
            },
        );
    }
    Ok(out)
}

/// Merge defaults + yaml + env + caller overrides into the effective decay map.
/// Later sources win. Mirrors TS `resolveRecencyDecayMap`.
pub fn resolve_recency_decay_map(
    yaml: Option<&serde_yaml::Value>,
    env_value: Option<&str>,
    caller: Option<&RecencyDecayMap>,
) -> Result<RecencyDecayMap, RecencyDecayParseError> {
    let mut out = default_recency_decay();
    out.extend(parse_recency_decay_yaml(yaml)?);
    out.extend(parse_recency_decay_env(env_value)?);
    if let Some(c) = caller {
        out.extend(c.iter().map(|(k, v)| (k.clone(), *v)));
    }
    Ok(out)
}

/// One row's mutable recency-boost view. The engine adapts its `SearchResult`
/// rows to this so [`apply_recency_boost`] stays independent of the engine type
/// and can be shared across the `InMemory` / libsql / postgres backends.
pub struct RecencyRow<'a> {
    /// Slug used for prefix matching (mirrors TS `r.slug`).
    pub slug: &'a str,
    /// Lookup key `"{source_id}::{slug}"` into the `dates` map.
    pub key: String,
    /// Current (possibly already-boosted) score. Mutated in place.
    pub score: &'a mut f64,
    /// Recency stamp slot. Set to `Some(factor)` only when a boost is applied.
    pub recency_boost: &'a mut Option<f64>,
}

const MS_PER_DAY: f64 = 86_400_000.0;

/// Apply the per-prefix recency boost in place. Mirrors TS `applyRecencyBoost`
/// (`src/core/search/hybrid.ts:185`). Pure: `now_ms` and `dates` are injected,
/// so tests are fully deterministic. The caller re-sorts afterwards.
///
/// For each row: skip if score is non-finite or (when `floor` is set) below it;
/// look up its effective date (skip if absent); resolve the longest matching
/// prefix (falling back to `fallback`); skip evergreen entries
/// (`halflife_days == 0` or `coefficient == 0`); otherwise multiply score by
/// `1 + strength_mul * coefficient * halflife / (halflife + days_old)` and stamp
/// `recency_boost`.
pub fn apply_recency_boost(
    rows: &mut [RecencyRow<'_>],
    dates: &HashMap<String, i64>,
    strength: RecencyStrength,
    decay_map: &RecencyDecayMap,
    fallback: RecencyDecayConfig,
    now_ms: i64,
    floor: Option<f64>,
) {
    let strength_mul = strength.multiplier();
    // Sort prefixes longest-first so `media/articles/` wins over `media/`.
    let mut prefixes: Vec<&String> = decay_map.keys().collect();
    prefixes.sort_by_key(|p| std::cmp::Reverse(p.len()));

    for row in rows.iter_mut() {
        if !row.score.is_finite() {
            continue;
        }
        if let Some(f) = floor {
            if *row.score < f {
                continue;
            }
        }
        let Some(&date_ms) = dates.get(&row.key) else {
            continue;
        };
        let days_old = ((now_ms - date_ms) as f64 / MS_PER_DAY).max(0.0);

        // First (longest) matching prefix, else fallback.
        let cfg = prefixes
            .iter()
            .find(|p| row.slug.starts_with(p.as_str()))
            .map_or(fallback, |p| decay_map[*p]);

        if cfg.halflife_days == 0.0 || cfg.coefficient == 0.0 {
            continue; // evergreen
        }
        let recency_component =
            cfg.coefficient * cfg.halflife_days / (cfg.halflife_days + days_old);
        let factor = 1.0 + strength_mul * recency_component;
        *row.score *= factor;
        *row.recency_boost = Some(factor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(hl: f64, coef: f64) -> RecencyDecayConfig {
        RecencyDecayConfig {
            halflife_days: hl,
            coefficient: coef,
        }
    }

    // ── env parser ──────────────────────────────────────────────────────
    #[test]
    fn env_none_is_empty() {
        assert!(parse_recency_decay_env(None).unwrap().is_empty());
    }

    #[test]
    fn env_parses_triples_with_slash_prefix() {
        let m = parse_recency_decay_env(Some("daily/:7:2.0,media/x/:14:1.5")).unwrap();
        assert_eq!(m["daily/"], cfg(7.0, 2.0));
        assert_eq!(m["media/x/"], cfg(14.0, 1.5));
    }

    #[test]
    fn env_fails_loud_on_missing_field() {
        let e = parse_recency_decay_env(Some("daily/:7")).unwrap_err();
        assert_eq!(e.source, RecencyDecaySource::Env);
    }

    #[test]
    fn env_fails_loud_on_negative_halflife() {
        assert!(parse_recency_decay_env(Some("daily/:-1:2")).is_err());
    }

    #[test]
    fn env_fails_loud_on_nonnumeric_coefficient() {
        assert!(parse_recency_decay_env(Some("daily/:7:abc")).is_err());
    }

    // ── yaml parser ─────────────────────────────────────────────────────
    #[test]
    fn yaml_missing_recency_is_empty() {
        let v: serde_yaml::Value = serde_yaml::from_str("other: 1").unwrap();
        assert!(parse_recency_decay_yaml(Some(&v)).unwrap().is_empty());
    }

    #[test]
    fn yaml_parses_entries() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            "recency:\n  daily/: { halflifeDays: 14, coefficient: 1.5 }\n  concepts/: { halflifeDays: 0, coefficient: 0 }",
        )
        .unwrap();
        let m = parse_recency_decay_yaml(Some(&v)).unwrap();
        assert_eq!(m["daily/"], cfg(14.0, 1.5));
        assert_eq!(m["concepts/"], cfg(0.0, 0.0));
    }

    #[test]
    fn yaml_fails_loud_on_non_map_recency() {
        let v: serde_yaml::Value = serde_yaml::from_str("recency: 5").unwrap();
        assert!(parse_recency_decay_yaml(Some(&v)).is_err());
    }

    #[test]
    fn yaml_fails_loud_on_bad_entry_field() {
        let v: serde_yaml::Value =
            serde_yaml::from_str("recency:\n  daily/: { halflifeDays: -1, coefficient: 1 }")
                .unwrap();
        assert!(parse_recency_decay_yaml(Some(&v)).is_err());
    }

    // ── resolve merge (later wins) ──────────────────────────────────────
    #[test]
    fn resolve_layers_env_over_yaml_over_default() {
        let yaml: serde_yaml::Value =
            serde_yaml::from_str("recency:\n  daily/: { halflifeDays: 5, coefficient: 5 }")
                .unwrap();
        let m = resolve_recency_decay_map(Some(&yaml), Some("daily/:9:9"), None).unwrap();
        // env wins over yaml wins over default.
        assert_eq!(m["daily/"], cfg(9.0, 9.0));
        // untouched default survives.
        assert_eq!(m["concepts/"], cfg(0.0, 0.0));
    }

    #[test]
    fn resolve_caller_wins_last() {
        let mut caller = RecencyDecayMap::new();
        caller.insert("daily/".to_string(), cfg(1.0, 1.0));
        let m = resolve_recency_decay_map(None, Some("daily/:9:9"), Some(&caller)).unwrap();
        assert_eq!(m["daily/"], cfg(1.0, 1.0));
    }

    // ── apply_recency_boost (deterministic via injected now_ms) ─────────
    /// Helper: run the boost over owned (score, stamp) tuples keyed by slug.
    fn run(
        rows: &mut [(String, f64, Option<f64>)],
        dates: &HashMap<String, i64>,
        map: &RecencyDecayMap,
        fallback: RecencyDecayConfig,
        now_ms: i64,
        floor: Option<f64>,
    ) {
        let mut views: Vec<RecencyRow<'_>> = rows
            .iter_mut()
            .map(|(slug, score, stamp)| RecencyRow {
                slug: slug.as_str(),
                key: format!("default::{slug}"),
                score,
                recency_boost: stamp,
            })
            .collect();
        apply_recency_boost(
            &mut views,
            dates,
            RecencyStrength::On,
            map,
            fallback,
            now_ms,
            floor,
        );
    }

    #[test]
    fn boost_exact_factor_at_one_halflife() {
        // daily/ hl=14 coef=1.5; days_old = 14 => component = coef/2 = 0.75;
        // strength 'on' => factor = 1 + 0.75 = 1.75.
        let now_ms = 14 * 86_400_000_i64;
        let mut dates = HashMap::new();
        dates.insert("default::daily/mon".to_string(), 0_i64); // 14 days old
        let mut rows = vec![("daily/mon".to_string(), 1.0_f64, None)];
        run(
            &mut rows,
            &dates,
            &default_recency_decay(),
            DEFAULT_FALLBACK,
            now_ms,
            None,
        );
        let expected = 1.0 + 1.5 * 14.0 / (14.0 + 14.0);
        assert!((rows[0].1 - expected).abs() < 1e-9, "score {}", rows[0].1);
        assert!((rows[0].2.unwrap() - expected).abs() < 1e-9);
        assert!((expected - 1.75).abs() < 1e-9);
    }

    #[test]
    fn boost_longest_prefix_wins() {
        // media/articles/ (hl=90 coef=0.5) must beat a shorter media/ override.
        let mut map = default_recency_decay();
        map.insert("media/".to_string(), cfg(1.0, 9.0)); // would give a huge factor
        let now_ms = 0_i64;
        let mut dates = HashMap::new();
        dates.insert("default::media/articles/x".to_string(), 0_i64); // days_old 0
        let mut rows = vec![("media/articles/x".to_string(), 1.0_f64, None)];
        run(&mut rows, &dates, &map, DEFAULT_FALLBACK, now_ms, None);
        // days_old 0 => component = coefficient = 0.5 => factor 1.5 (articles),
        // NOT 1 + 9 = 10 (the shorter media/ prefix).
        assert!((rows[0].1 - 1.5).abs() < 1e-9, "score {}", rows[0].1);
    }

    #[test]
    fn boost_evergreen_prefix_skipped() {
        // concepts/ hl=0 => evergreen => no boost, no stamp.
        let mut dates = HashMap::new();
        dates.insert("default::concepts/x".to_string(), 0_i64);
        let mut rows = vec![("concepts/x".to_string(), 1.0_f64, None)];
        run(
            &mut rows,
            &dates,
            &default_recency_decay(),
            DEFAULT_FALLBACK,
            0,
            None,
        );
        assert_eq!(rows[0].1, 1.0);
        assert_eq!(rows[0].2, None);
    }

    #[test]
    fn boost_no_date_entry_skipped() {
        let dates = HashMap::new(); // empty => no date for the row
        let mut rows = vec![("daily/x".to_string(), 1.0_f64, None)];
        run(
            &mut rows,
            &dates,
            &default_recency_decay(),
            DEFAULT_FALLBACK,
            0,
            None,
        );
        assert_eq!(rows[0].1, 1.0);
        assert_eq!(rows[0].2, None);
    }

    #[test]
    fn boost_floor_gate_skips_low_score_row() {
        let mut dates = HashMap::new();
        dates.insert("default::daily/x".to_string(), 0_i64);
        // score 0.5 below floor 0.9 => skipped.
        let mut rows = vec![("daily/x".to_string(), 0.5_f64, None)];
        run(
            &mut rows,
            &dates,
            &default_recency_decay(),
            DEFAULT_FALLBACK,
            0,
            Some(0.9),
        );
        assert_eq!(rows[0].1, 0.5);
        assert_eq!(rows[0].2, None);
    }

    #[test]
    fn boost_fallback_used_for_unmatched_prefix() {
        // Slug matches no default prefix => DEFAULT_FALLBACK (hl=90 coef=0.5).
        let mut dates = HashMap::new();
        dates.insert("default::random/x".to_string(), 0_i64); // days_old 0
        let mut rows = vec![("random/x".to_string(), 1.0_f64, None)];
        run(
            &mut rows,
            &dates,
            &default_recency_decay(),
            DEFAULT_FALLBACK,
            0,
            None,
        );
        // days_old 0 => component = coefficient = 0.5 => factor 1.5.
        assert!((rows[0].1 - 1.5).abs() < 1e-9);
    }
}
