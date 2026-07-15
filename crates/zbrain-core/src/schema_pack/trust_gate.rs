//! Trust gate — validates per-call schema_pack parameter against remote boundary.
//!
//! Ported from TS `src/core/schema-pack/op-trust-gate.ts`.
//!
//! Fail-closed: any `remote` value that is not strictly `false` is treated
//! as remote/untrusted. Per-call schema_pack is only allowed from local callers.

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SchemaPackTrustGateError {
    pub message: String,
}

impl std::fmt::Display for SchemaPackTrustGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SchemaPackTrustGateError: {}", self.message)
    }
}

impl std::error::Error for SchemaPackTrustGateError {}

// ---------------------------------------------------------------------------
// Trust gate context (minimal — full OperationContext wired in 1-6)
// ---------------------------------------------------------------------------

/// Minimal context for trust gate validation. In the full system, this is
/// extracted from `OperationContext`.
#[derive(Debug, Clone, Default)]
pub struct TrustGateContext {
    /// `false` = local/CLI caller (trusted). Any other value = remote (untrusted).
    pub remote: bool,
    /// Optional source ID for federated reads.
    pub source_id: Option<String>,
    /// Optional allowed source IDs for multi-source federated reads.
    pub allowed_source_ids: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// validate_schema_pack_trust_gate
// ---------------------------------------------------------------------------

/// Validate the per-call `schema_pack` parameter.
///
/// Returns:
/// - `Ok(None)` — parameter not set (no-op)
/// - `Ok(Some(name))` — validated pack name (local caller only)
/// - `Err(SchemaPackTrustGateError)` — remote caller attempted per-call override
pub fn validate_schema_pack_trust_gate(
    ctx: &TrustGateContext,
    schema_pack_param: Option<&str>,
) -> Result<Option<String>, SchemaPackTrustGateError> {
    let param = match schema_pack_param {
        None => return Ok(None),
        Some(s) => s.trim(),
    };

    if param.is_empty() {
        return Ok(None);
    }

    if ctx.remote {
        return Err(SchemaPackTrustGateError {
            message: format!(
                "per-call schema_pack parameter is not allowed for remote callers; \
                 use zbrain.yml, config.json, ZBRAIN_SCHEMA_PACK env var, or `zbrain config set`"
            ),
        });
    }

    Ok(Some(param.to_string()))
}

// ---------------------------------------------------------------------------
// Federated read divergence check
// ---------------------------------------------------------------------------

/// Check if multiple sources resolve to the same pack name.
/// Returns the single source_id + pack_name if all agree, or an error if they diverge.
///
/// This is the core of the v0.39 T19 federated read divergence detection.
pub fn check_federated_divergence(
    source_ids: &[String],
    resolve_fn: &dyn Fn(&str) -> String,
) -> Result<(String, String), SchemaPackTrustGateError> {
    if source_ids.is_empty() {
        return Err(SchemaPackTrustGateError {
            message: "federated read with no sources".into(),
        });
    }

    if source_ids.len() == 1 {
        let pack_name = resolve_fn(&source_ids[0]);
        return Ok((source_ids[0].clone(), pack_name));
    }

    // Multi-source: check divergence
    let mut packs: Vec<(String, String)> = Vec::new();
    for sid in source_ids {
        let pack_name = resolve_fn(sid);
        packs.push((sid.clone(), pack_name));
    }

    let unique_packs: std::collections::HashSet<&str> =
        packs.iter().map(|(_, p)| p.as_str()).collect();

    if unique_packs.len() > 1 {
        let summary: Vec<String> = packs
            .iter()
            .map(|(sid, pack)| format!("{sid}={pack}"))
            .collect();
        return Err(SchemaPackTrustGateError {
            message: format!(
                "federated read across {} sources resolved to {} different packs: [{}]; \
                 refusing to select one arbitrarily",
                source_ids.len(),
                unique_packs.len(),
                summary.join(", ")
            ),
        });
    }

    // All agree — use the first source_id
    Ok((packs[0].0.clone(), packs[0].1.clone()))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- validate_schema_pack_trust_gate --------------------------------

    #[test]
    fn none_param_returns_none() {
        let ctx = TrustGateContext { remote: true, ..Default::default() };
        let result = validate_schema_pack_trust_gate(&ctx, None);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn empty_param_returns_none() {
        let ctx = TrustGateContext { remote: true, ..Default::default() };
        let result = validate_schema_pack_trust_gate(&ctx, Some(""));
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn whitespace_param_returns_none() {
        let ctx = TrustGateContext { remote: true, ..Default::default() };
        let result = validate_schema_pack_trust_gate(&ctx, Some("   "));
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn local_caller_can_set_per_call() {
        let ctx = TrustGateContext { remote: false, ..Default::default() };
        let result = validate_schema_pack_trust_gate(&ctx, Some("my-pack"));
        assert_eq!(result.unwrap(), Some("my-pack".to_string()));
    }

    #[test]
    fn remote_caller_blocked() {
        let ctx = TrustGateContext { remote: true, ..Default::default() };
        let err = validate_schema_pack_trust_gate(&ctx, Some("my-pack")).unwrap_err();
        assert!(err.message.contains("not allowed for remote callers"));
    }

    // ---- check_federated_divergence -------------------------------------

    #[test]
    fn single_source_no_divergence() {
        let sources = vec!["s1".to_string()];
        let (sid, pack) = check_federated_divergence(&sources, &|_| "zbrain-base".into()).unwrap();
        assert_eq!(sid, "s1");
        assert_eq!(pack, "zbrain-base");
    }

    #[test]
    fn multi_source_same_pack_no_divergence() {
        let sources = vec!["s1".into(), "s2".into(), "s3".into()];
        let (sid, pack) = check_federated_divergence(&sources, &|_| "zbrain-base".into()).unwrap();
        assert_eq!(sid, "s1"); // Uses first source
        assert_eq!(pack, "zbrain-base");
    }

    #[test]
    fn multi_source_divergence_rejected() {
        let sources = vec!["s1".into(), "s2".into()];
        let resolve = |sid: &str| -> String {
            match sid {
                "s1" => "pack-a".into(),
                "s2" => "pack-b".into(),
                _ => "zbrain-base".into(),
            }
        };
        let err = check_federated_divergence(&sources, &resolve).unwrap_err();
        assert!(err.message.contains("2 different packs"));
        assert!(err.message.contains("s1=pack-a"));
        assert!(err.message.contains("s2=pack-b"));
    }

    #[test]
    fn empty_sources_rejected() {
        let sources: Vec<String> = vec![];
        let err = check_federated_divergence(&sources, &|_| "x".into()).unwrap_err();
        assert!(err.message.contains("no sources"));
    }
}
