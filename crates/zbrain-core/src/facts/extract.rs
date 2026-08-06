//! v0.31 Hot Memory — turn-extractor config surface (Rust port of
//! `src/core/facts/extract.ts`, the *non-LLM* parts).
//!
//! The LLM extraction itself (`extractFactsFromTurn`) is already ported in
//! `crate::autopilot::phases::conversation_facts_backfill`. This module
//! covers the pieces that were still "config read not yet ported": the
//! kill-switch, the model resolver, and the strict-JSON parser.

use crate::ai::model_config::{resolve_model, ConfigLookup, ModelTier, ResolveModelOpts};
use crate::engine::BrainEngine;
use crate::types::FactKind;

/// All fact kinds the extractor can emit. Mirrors TS `ALL_EXTRACT_KINDS`.
pub const ALL_EXTRACT_KINDS: &[FactKind] = &[
    FactKind::Event,
    FactKind::Preference,
    FactKind::Commitment,
    FactKind::Belief,
    FactKind::Fact,
];

/// v0.31 (D15): kill-switch for fact extraction.
///
/// Reads the `facts.extraction_enabled` config row. Defaults to `true` (on by
/// default — the headline feature ships enabled). Operators flip it to
/// `false`/`0`/`no`/`off` (case-insensitive) to disable extraction across the
/// brain without a binary downgrade.
pub async fn is_facts_extraction_enabled(engine: &dyn BrainEngine) -> crate::Result<bool> {
    let val = engine.get_config("facts.extraction_enabled").await?;
    Ok(facts_extraction_enabled_from_config(val.as_deref()))
}

/// Pure truthiness check for the `facts.extraction_enabled` config value.
/// `None` (unset) → enabled (on by default). Case-insensitive match against
/// `false` / `0` / `no` / `off` disables.
#[must_use]
pub fn facts_extraction_enabled_from_config(raw: Option<&str>) -> bool {
    match raw {
        None => true,
        Some(v) => {
            let normalized = v.trim().to_lowercase();
            !["false", "0", "no", "off"].contains(&normalized.as_str())
        }
    }
}

/// Get the configured model for facts extraction. Defaults to Sonnet since
/// notability/salience judgment requires a sophisticated model, not Haiku.
/// Configurable via `zbrain config set facts.extraction_model <model>`.
///
/// Takes a [`ConfigLookup`] (typically the snapshot from
/// `prefetch_model_lookup`) rather than the engine, matching how the rest of
/// the Rust model-resolution code resolves tiers.
#[must_use]
pub fn get_facts_extraction_model(lookup: &dyn ConfigLookup) -> String {
    let resolved = resolve_model(
        lookup,
        &ResolveModelOpts {
            config_key: Some("facts.extraction_model".to_string()),
            tier: Some(ModelTier::Reasoning),
            fallback: "anthropic:claude-sonnet-4-6".to_string(),
            ..Default::default()
        },
    );
    if resolved.contains(':') {
        resolved
    } else {
        format!("anthropic:{resolved}")
    }
}

/// A pre-parse extracted candidate (mirrors the TS `RawExtracted` shape).
#[derive(Debug, Clone, PartialEq)]
pub struct RawExtracted {
    pub fact: String,
    pub kind: String,
    pub entity: Option<String>,
    pub confidence: f64,
    pub notability: Option<String>,
    /// v0.35.4 (D-CDX-2) — typed-claim fields.
    pub metric: Option<String>,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub period: Option<String>,
}

/// Parse the LLM's strict-JSON output into a list of raw extracted
/// candidates. 4-strategy fallback for malformed responses. Mirrors TS
/// `parseExtractorJson` (production callers use the full
/// `extractFactsFromTurn` in `conversation_facts_backfill`).
#[must_use]
pub fn parse_extractor_json(raw: &str) -> Option<Vec<RawExtracted>> {
    let cleaned = raw
        .trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw)
        .trim();
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    if let Some(out) = try_array_shape(cleaned) {
        return Some(out);
    }
    // Substring scan for an embedded `{"facts":[...]}` shape.
    if let Some(m) = cleaned.find("{\"facts\"") {
        if let Some(end) = cleaned[m..].rfind('}') {
            if let Some(out) = try_array_shape(&cleaned[m..=m + end]) {
                return Some(out);
            }
        }
    }
    None
}

fn try_array_shape(s: &str) -> Option<Vec<RawExtracted>> {
    let parsed: serde_json::Value = serde_json::from_str(s).ok()?;
    let arr = parsed.get("facts")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let fact = item.get("fact")?.as_str()?;
        let kind = item.get("kind")?.as_str()?;
        if fact.is_empty() || kind.is_empty() {
            continue;
        }
        let confidence = item
            .get("confidence")
            .and_then(|v| v.as_f64())
            .filter(|c| c.is_finite())
            .unwrap_or(1.0);
        let notability = item
            .get("notability")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let metric = item
            .get("metric")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let value = item
            .get("value")
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite());
        let unit = item
            .get("unit")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let period = item
            .get("period")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.push(RawExtracted {
            fact: fact.to_string(),
            kind: kind.to_string(),
            entity: item
                .get("entity")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            confidence,
            notability,
            metric,
            value,
            unit,
            period,
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_when_unset() {
        assert!(facts_extraction_enabled_from_config(None));
        assert!(facts_extraction_enabled_from_config(Some("")));
        assert!(facts_extraction_enabled_from_config(Some("true")));
    }

    #[test]
    fn disabled_when_off_variants() {
        for v in ["false", "FALSE", "0", "No", "off", "OFF", " false ", "no"] {
            assert!(!facts_extraction_enabled_from_config(Some(v)), "expected '{v}' to disable");
        }
    }

    #[test]
    fn parses_strict_array() {
        let raw = r#"{"facts":[{"fact":"I quit coffee","kind":"preference","confidence":1.0,"notability":"high"}]}"#;
        let out = parse_extractor_json(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].fact, "I quit coffee");
        assert_eq!(out[0].kind, "preference");
        assert_eq!(out[0].confidence, 1.0);
        assert_eq!(out[0].notability.as_deref(), Some("high"));
    }

    #[test]
    fn parses_with_code_fence() {
        let raw = "```json\n{\"facts\":[{\"fact\":\"x\",\"kind\":\"fact\"}]}\n```";
        let out = parse_extractor_json(raw).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn parses_embedded_object() {
        let raw = "Sure! {\"facts\":[{\"fact\":\"a\",\"kind\":\"event\"}]} done";
        let out = parse_extractor_json(raw).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse_extractor_json("not json at all").is_none());
    }

    #[test]
    fn typed_claim_fields_threaded() {
        let raw = r#"{"facts":[{"fact":"MRR","kind":"fact","metric":"mrr","value":50000,"unit":"USD","period":"monthly"}]}"#;
        let out = parse_extractor_json(raw).unwrap();
        assert_eq!(out[0].metric.as_deref(), Some("mrr"));
        assert_eq!(out[0].value, Some(50000.0));
        assert_eq!(out[0].unit.as_deref(), Some("USD"));
        assert_eq!(out[0].period.as_deref(), Some("monthly"));
    }
}
