//! Part12 1-3-1 — port of `src/core/cycle/synthesize-concepts.ts` →
//! `autopilot/phases/synthesize_concepts.rs` (v0.41 T6 minimal-viable).
//!
//! Groups atom-typed pages by their `concepts:` frontmatter refs, tiers
//! groups by count (T1 ≥10, T2 ≥5, T3 ≥2), LLM-synthesizes a 1-paragraph
//! narrative for T1/T2 (budget-capped, deterministic template fallback on
//! failure/budget), deterministic narrative for T3, then writes
//! `type: "concept"` pages under `concepts/{name}`.
//!
//! Atom discovery uses `BrainEngine::execute_raw` (fail-soft: engines
//! without raw SQL — e.g. InMemory — degrade to zero atoms → phase skips
//! cleanly, mirroring the TS `catch {}` no-op). Tests inject atoms via
//! `SynthesizeConceptsOpts::atoms` (mirrors the TS `_atoms` seam).

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::engine::BrainEngine;
use crate::error::Result as ZbResult;
use crate::types::PageType;
use crate::PageInput;
use serde_json::json;

const DEFAULT_BUDGET_USD: f64 = 1.5;
const TIER_T1_MIN: usize = 10;
const TIER_T2_MIN: usize = 5;
const TIER_T3_MIN: usize = 2;

const SYNTH_PROMPT: &str = r#"You write a 1-paragraph executive summary of a concept
based on multiple atom-shaped insights that reference it.

Output ONLY the summary paragraph (3-5 sentences). No headers, no JSON,
no preamble. Write in plain English, present-tense voice. Synthesize what
the atoms collectively SAY about the concept; don't enumerate the atoms."#;

/// One atom candidate for concept grouping. Mirrors the TS `_atoms` seam
/// element `{ slug, title, body, concept_refs }`.
#[derive(Debug, Clone)]
pub struct AtomForConcepts {
    pub slug: String,
    pub title: String,
    pub body: String,
    pub concept_refs: Vec<String>,
}

/// Options for [`run_synthesize_concepts`]. Mirrors TS `SynthesizeConceptsOpts`.
#[derive(Debug, Clone, Default)]
pub struct SynthesizeConceptsOpts {
    pub dry_run: bool,
    /// Source the concept pages are written under. Defaults to `"default"`.
    pub source_id: Option<String>,
    /// Test seam: skip the DB query and cluster these atoms directly.
    /// `None` → query the engine; `Some(vec![])` → zero atoms (skip).
    pub atoms: Option<Vec<AtomForConcepts>>,
}

/// A single failed LLM synthesis (fell back to the deterministic template).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConceptSynthesisFailure {
    pub concept: String,
    pub error: String,
}

/// Result of a `synthesize_concepts` run. Mirrors the TS `PhaseResult`
/// summary/details shape.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SynthesizeConceptsResult {
    /// `"ok"`, `"warn"` or `"skipped"`.
    pub status: String,
    pub summary: String,
    pub reason: Option<String>,
    pub concepts_written: u64,
    pub tier_t1: u64,
    pub tier_t2: u64,
    pub tier_t3: u64,
    pub groups_found: u64,
    pub atoms_seen: u64,
    pub failures: Vec<ConceptSynthesisFailure>,
    pub estimated_spend_usd: f64,
    pub budget_usd: f64,
    pub dry_run: bool,
}

/// One concept group after tier assignment.
struct AtomGroup {
    concept_slug: String,
    atom_titles: Vec<String>,
    atom_bodies: Vec<String>,
    /// `"T1"`, `"T2"` or `"T3"` (T4 unreachable — the `≥2` filter and tier
    /// thresholds never assign it; kept out for simplicity, mirroring the
    /// TS runtime behavior).
    tier: &'static str,
}

/// Deterministic fallback narrative for T3 concepts and budget-exhausted /
/// LLM-failed T1/T2 groups. Mirrors TS `deterministicNarrative`.
fn deterministic_narrative(group: &AtomGroup) -> String {
    let count = group.atom_titles.len();
    format!(
        "{} concept. {} atom{} reference this. Top mentions:\n{}",
        group.tier,
        count,
        if count == 1 { "" } else { "s" },
        group
            .atom_titles
            .iter()
            .take(5)
            .map(|t| format!("  - {}", t))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Load atom pages with concept refs via raw SQL. Fail-soft: any error
/// (e.g. `execute_raw` unsupported on InMemory) degrades to an empty list,
/// mirroring the TS `catch {}` clean no-op.
async fn load_atoms(engine: &dyn BrainEngine) -> Vec<AtomForConcepts> {
    let sql = "SELECT slug, title, compiled_truth, frontmatter \
                 FROM pages \
                WHERE type = 'atom' \
                  AND deleted_at IS NULL \
                  AND (frontmatter->>'imported_from') IS NULL";
    let rows = match engine.execute_raw(sql, &[]).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut atoms = Vec::new();
    for row in rows {
        let slug = row.get("slug").and_then(|v| v.as_str()).unwrap_or_default();
        let title = row.get("title").and_then(|v| v.as_str()).unwrap_or_default();
        let body = row
            .get("compiled_truth")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        // frontmatter arrives as a JSON object (postgres JSONB) or a JSON
        // text blob (libsql TEXT column) — accept both.
        let fm: Option<serde_json::Value> = match row.get("frontmatter") {
            Some(serde_json::Value::Object(o)) => Some(serde_json::Value::Object(o.clone())),
            Some(serde_json::Value::String(s)) => serde_json::from_str(s).ok(),
            _ => None,
        };
        let concept_refs: Vec<String> = fm
            .as_ref()
            .and_then(|f| f.get("concepts"))
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if concept_refs.is_empty() {
            continue;
        }
        atoms.push(AtomForConcepts {
            slug: slug.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            concept_refs,
        });
    }
    atoms
}

/// Run the synthesize-concepts phase.
///
/// - Atoms come from `opts.atoms` (test seam) or a raw-SQL page query.
/// - Groups with `< 2` atoms are dropped; tiers by count (T1 ≥10, T2 ≥5,
///   T3 ≥2). Insertion order is preserved (budget cutoff is order-dependent,
///   mirroring the TS `Map` iteration).
/// - T1/T2 → one Sonnet call each while under `DEFAULT_BUDGET_USD`; failures
///   and budget overruns fall back to [`deterministic_narrative`].
/// - Writes `concepts/{name}` pages unless `dry_run`.
pub async fn run_synthesize_concepts(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    opts: &SynthesizeConceptsOpts,
) -> ZbResult<SynthesizeConceptsResult> {
    let source_id = opts
        .source_id
        .clone()
        .unwrap_or_else(|| "default".to_string());

    // 1. Get atom pages (test seam OR DB query).
    let atoms = match &opts.atoms {
        Some(seam) => seam.clone(),
        None => load_atoms(engine).await,
    };

    if atoms.is_empty() {
        return Ok(SynthesizeConceptsResult {
            status: "skipped".into(),
            summary: "synthesize_concepts: no atoms with concept refs".into(),
            reason: Some("no_atoms".into()),
            budget_usd: DEFAULT_BUDGET_USD,
            dry_run: opts.dry_run,
            ..Default::default()
        });
    }

    // 2. Group atoms by concept slug (insertion-ordered, mirrors TS Map).
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, (Vec<String>, Vec<String>)> =
        std::collections::HashMap::new();
    for atom in &atoms {
        for concept_slug in &atom.concept_refs {
            let entry = groups.entry(concept_slug.clone()).or_insert_with(|| {
                order.push(concept_slug.clone());
                (Vec::new(), Vec::new())
            });
            entry.0.push(atom.title.clone());
            entry.1.push(atom.body.clone());
        }
    }

    // 3. Filter to count ≥2, assign tier.
    let mut atom_groups: Vec<AtomGroup> = Vec::new();
    for concept_slug in &order {
        let (titles, bodies) = &groups[concept_slug];
        let count = titles.len();
        if count < TIER_T3_MIN {
            continue;
        }
        let tier = if count >= TIER_T1_MIN {
            "T1"
        } else if count >= TIER_T2_MIN {
            "T2"
        } else {
            "T3"
        };
        atom_groups.push(AtomGroup {
            concept_slug: concept_slug.clone(),
            atom_titles: titles.clone(),
            atom_bodies: bodies.clone(),
            tier,
        });
    }

    if atom_groups.is_empty() {
        return Ok(SynthesizeConceptsResult {
            status: "skipped".into(),
            summary: format!(
                "synthesize_concepts: no concept groups with ≥{} atoms",
                TIER_T3_MIN
            ),
            reason: Some("no_groups_above_threshold".into()),
            atoms_seen: atoms.len() as u64,
            budget_usd: DEFAULT_BUDGET_USD,
            dry_run: opts.dry_run,
            ..Default::default()
        });
    }

    // 4. Per group: synthesize narrative (LLM for T1/T2, deterministic for T3).
    let mut result = SynthesizeConceptsResult {
        atoms_seen: atoms.len() as u64,
        groups_found: atom_groups.len() as u64,
        budget_usd: DEFAULT_BUDGET_USD,
        dry_run: opts.dry_run,
        ..Default::default()
    };

    for group in &atom_groups {
        match group.tier {
            "T1" => result.tier_t1 += 1,
            "T2" => result.tier_t2 += 1,
            _ => result.tier_t3 += 1,
        }

        let narrative: String = if group.tier == "T1" || group.tier == "T2" {
            if result.estimated_spend_usd >= DEFAULT_BUDGET_USD {
                deterministic_narrative(group)
            } else {
                let user_content = format!(
                    "Concept slug: {}\n{} atoms reference this concept.\n\nSample atom titles:\n{}\n\nSample atom bodies:\n{}",
                    group.concept_slug,
                    group.atom_titles.len(),
                    group
                        .atom_titles
                        .iter()
                        .take(10)
                        .map(|t| format!("  - {}", t))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    group
                        .atom_bodies
                        .iter()
                        .take(5)
                        .enumerate()
                        .map(|(i, b)| format!("{}. {}", i + 1, b.chars().take(500).collect::<String>()))
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                );
                match chat
                    .chat(ChatOpts {
                        model: None,
                        system: Some(SYNTH_PROMPT.to_string()),
                        messages: vec![ChatMessage::text(ChatRole::User, user_content)],
                        tools: vec![],
                        max_tokens: Some(500),
                        cache_system: false,
                    })
                    .await
                {
                    Ok(r) => {
                        // Sonnet at ~$3/M input + $15/M output.
                        result.estimated_spend_usd += (r.usage.input_tokens as f64 * 3.0
                            + r.usage.output_tokens as f64 * 15.0)
                            / 1_000_000.0;
                        let text = r.text.trim().to_string();
                        if text.is_empty() {
                            deterministic_narrative(group)
                        } else {
                            text
                        }
                    }
                    Err(e) => {
                        result.failures.push(ConceptSynthesisFailure {
                            concept: group.concept_slug.clone(),
                            error: e.to_string(),
                        });
                        deterministic_narrative(group)
                    }
                }
            }
        } else {
            deterministic_narrative(group)
        };

        if !opts.dry_run {
            let title = group
                .concept_slug
                .rsplit('/')
                .next()
                .unwrap_or(&group.concept_slug)
                .to_string();
            let input = PageInput {
                page_type: PageType::from("concept"),
                title: title.replace('-', " "),
                compiled_truth: narrative,
                timeline: Some(String::new()),
                frontmatter: Some(json!({
                    "type": "concept",
                    "tier": group.tier,
                    "mention_count": group.atom_titles.len(),
                    "composite_score": group.atom_titles.len(),
                    "synthesized_at": chrono::Utc::now().to_rfc3339(),
                    "synthesized_by": "synthesize_concepts-v0.41",
                })),
                ..Default::default()
            };
            engine
                .put_page(&format!("concepts/{}", title), Some(&source_id), &input)
                .await?;
        }
        result.concepts_written += 1;
    }

    result.status = if result.failures.is_empty() {
        "ok".into()
    } else {
        "warn".into()
    };
    result.summary = format!(
        "synthesize_concepts: {} concepts (T1={} T2={} T3={}){}",
        result.concepts_written,
        result.tier_t1,
        result.tier_t2,
        result.tier_t3,
        if result.failures.is_empty() {
            String::new()
        } else {
            format!(" ({} LLM-failed → template fallback)", result.failures.len())
        },
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatError, ChatResult, ChatUsage, StopReason};
    use crate::engine::{BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[derive(Debug)]
    struct StubChat {
        calls: Arc<AtomicU32>,
        text: String,
        usage: ChatUsage,
        fail: bool,
    }

    impl StubChat {
        fn ok(text: &str) -> (Self, Arc<AtomicU32>) {
            let calls = Arc::new(AtomicU32::new(0));
            (
                Self {
                    calls: calls.clone(),
                    text: text.to_string(),
                    usage: ChatUsage::default(),
                    fail: false,
                },
                calls,
            )
        }
    }

    #[async_trait::async_trait]
    impl crate::ai::chat::ChatProvider for StubChat {
        async fn chat(&self, _opts: ChatOpts) -> std::result::Result<ChatResult, ChatError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(ChatError::Transient {
                    message: "boom".into(),
                });
            }
            Ok(ChatResult {
                text: self.text.clone(),
                blocks: vec![],
                stop_reason: StopReason::End,
                usage: self.usage.clone(),
                model: "stub".to_string(),
                provider_id: "stub".to_string(),
                provider_metadata: None,
            })
        }
    }

    async fn engine() -> InMemoryEngine {
        let e = InMemoryEngine::new();
        e.connect(&EngineConfig::default()).await.unwrap();
        e
    }

    fn atom(title: &str, refs: &[&str]) -> AtomForConcepts {
        AtomForConcepts {
            slug: format!("atoms/{}", title),
            title: title.to_string(),
            body: format!("body of {}", title),
            concept_refs: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn skips_when_no_atoms() {
        let e = engine().await;
        let (chat, calls) = StubChat::ok("x");
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                atoms: Some(vec![]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("no_atoms"));
        assert_eq!(r.summary, "synthesize_concepts: no atoms with concept refs");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn inmemory_without_seam_fails_soft_to_no_atoms() {
        // InMemory has no execute_raw → load_atoms degrades to empty.
        let e = engine().await;
        let (chat, _) = StubChat::ok("x");
        let r = run_synthesize_concepts(&e, &chat, &SynthesizeConceptsOpts::default())
            .await
            .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("no_atoms"));
    }

    #[tokio::test]
    async fn skips_when_groups_below_threshold() {
        let e = engine().await;
        let (chat, calls) = StubChat::ok("x");
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                atoms: Some(vec![atom("a1", &["solo-concept"])]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("no_groups_above_threshold"));
        assert_eq!(r.atoms_seen, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn t3_group_writes_deterministic_concept_without_llm() {
        let e = engine().await;
        let (chat, calls) = StubChat::ok("SHOULD NOT BE USED");
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                atoms: Some(vec![
                    atom("a1", &["areas/psychology"]),
                    atom("a2", &["areas/psychology"]),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.concepts_written, 1);
        assert_eq!(r.tier_t3, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "T3 must not call the LLM");
        assert_eq!(r.summary, "synthesize_concepts: 1 concepts (T1=0 T2=0 T3=1)");

        // Nested concept slug → page under concepts/{last-segment}.
        let page = e
            .get_page("concepts/psychology", &GetPageOpts::default())
            .await
            .unwrap()
            .expect("concept page written");
        assert!(page.compiled_truth.starts_with("T3 concept. 2 atoms reference this."));
        let fm = &page.frontmatter;
        assert_eq!(fm["tier"], "T3");
        assert_eq!(fm["mention_count"], 2);
    }

    #[tokio::test]
    async fn t2_group_uses_llm_narrative() {
        let e = engine().await;
        let (chat, calls) = StubChat::ok("  A synthesized narrative.  ");
        let atoms: Vec<AtomForConcepts> =
            (0..5).map(|i| atom(&format!("a{}", i), &["deep-work"])).collect();
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                atoms: Some(atoms),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.tier_t2, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let page = e
            .get_page("concepts/deep-work", &GetPageOpts::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page.compiled_truth, "A synthesized narrative.");
        assert_eq!(page.title, "deep work");
    }

    #[tokio::test]
    async fn llm_failure_falls_back_to_template_and_warns() {
        let e = engine().await;
        let calls = Arc::new(AtomicU32::new(0));
        let chat = StubChat {
            calls: calls.clone(),
            text: String::new(),
            usage: ChatUsage::default(),
            fail: true,
        };
        let atoms: Vec<AtomForConcepts> =
            (0..5).map(|i| atom(&format!("a{}", i), &["c1"])).collect();
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                atoms: Some(atoms),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "warn");
        assert_eq!(r.failures.len(), 1);
        assert_eq!(r.failures[0].concept, "c1");
        assert_eq!(r.concepts_written, 1);
        assert!(r.summary.contains("(1 LLM-failed → template fallback)"));
        let page = e
            .get_page("concepts/c1", &GetPageOpts::default())
            .await
            .unwrap()
            .unwrap();
        assert!(page.compiled_truth.starts_with("T2 concept. 5 atoms"));
    }

    #[tokio::test]
    async fn budget_exhaustion_degrades_to_template() {
        let e = engine().await;
        let calls = Arc::new(AtomicU32::new(0));
        // 1M input tokens at $3/M → $3.0 spend after first call (> $1.5 cap).
        let chat = StubChat {
            calls: calls.clone(),
            text: "llm text".into(),
            usage: ChatUsage {
                input_tokens: 1_000_000,
                ..Default::default()
            },
            fail: false,
        };
        let mut atoms: Vec<AtomForConcepts> = Vec::new();
        for i in 0..5 {
            atoms.push(atom(&format!("x{}", i), &["first"]));
        }
        for i in 0..5 {
            atoms.push(atom(&format!("y{}", i), &["second"]));
        }
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                atoms: Some(atoms),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.concepts_written, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "second group must skip LLM");
        assert!(r.estimated_spend_usd >= DEFAULT_BUDGET_USD);
        // Insertion order preserved: "first" got the LLM text, "second" the template.
        let p1 = e.get_page("concepts/first", &GetPageOpts::default()).await.unwrap().unwrap();
        assert_eq!(p1.compiled_truth, "llm text");
        let p2 = e.get_page("concepts/second", &GetPageOpts::default()).await.unwrap().unwrap();
        assert!(p2.compiled_truth.starts_with("T2 concept."));
    }

    #[tokio::test]
    async fn dry_run_counts_but_does_not_write() {
        let e = engine().await;
        let (chat, _) = StubChat::ok("n");
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                dry_run: true,
                atoms: Some(vec![atom("a1", &["c"]), atom("a2", &["c"])]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.concepts_written, 1);
        assert!(r.dry_run);
        assert!(e
            .get_page("concepts/c", &GetPageOpts::default())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn t1_tier_assigned_at_ten_atoms() {
        let e = engine().await;
        let (chat, _) = StubChat::ok("t1 narrative");
        let atoms: Vec<AtomForConcepts> =
            (0..10).map(|i| atom(&format!("a{}", i), &["big"])).collect();
        let r = run_synthesize_concepts(
            &e,
            &chat,
            &SynthesizeConceptsOpts {
                atoms: Some(atoms),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.tier_t1, 1);
        let page = e.get_page("concepts/big", &GetPageOpts::default()).await.unwrap().unwrap();
        assert_eq!(page.frontmatter["tier"], "T1");
    }
}
