//! Part12 1-1-2 — port of `src/core/cycle/extract-atoms.ts` →
//! `autopilot/phases/extract_atoms.rs` (v0.41.2.1 rebuild).
//!
//! This phase extracts atomic content nuggets (atoms) from brain pages via
//! an LLM (Haiku). Atoms are written as `type: "atom"` pages. Pages are
//! discovered via a single NOT EXISTS SQL query (idempotent by
//! `content_hash`); the transcript path (filesystem `discoverTranscripts` +
//! `loadConfigWithEngine`) is **NOT** ported — registered in
//! docs/plans/MIGRATION.md (G62).
//!
//! Unlike `extract_facts` (fence parsing, no LLM), this phase calls the
//! chat provider. `run_extract_atoms` receives `&dyn ChatProvider` so tests
//! install a stub; production wires a real provider via `CycleOpts.chat`.

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::engine::BrainEngine;
use crate::error::{Error, Result as ZbResult};
use crate::time::today_utc_date;
use crate::types::{DiscoveredPage, PageType};
use crate::PageInput;
use serde_json::json;

const DEFAULT_BUDGET_USD: f64 = 0.3;
const PAGE_DISCOVERY_BUDGET: usize = 50;
const MIN_PAGE_CHARS_FOR_EXTRACTION: usize = 500;

/// Atom type vocabulary. Mirrors TS `ATOM_TYPES` (v0.42+ TODO: pull from
/// active pack manifest at runtime).
const ATOM_TYPES: &[&str] = &[
    "insight",
    "anecdote",
    "quote",
    "framework",
    "statistic",
    "story_angle",
    "strategy_angle",
    "strategy",
    "endorsement",
    "critique",
    "collection",
];

const EXTRACT_PROMPT: &str = r#"You extract atomic content nuggets from a transcript.

An atom is a single-source, self-contained idea that could become a tweet,
quote, or short essay angle. Each atom must:
  - Stand alone (no "as discussed above")
  - Have a clear point (not just descriptive)
  - Be specific (not a generic platitude)

Output a JSON array of atoms (1-3 per transcript, never more than 3).
Each atom: {title (≤80 chars), atom_type, body (2-4 sentences),
source_quote (verbatim ≤200 chars), lesson (one sentence), virality_score
(0-100), emotional_register (one of: shocking, inspiring, funny, sobering,
practical, controversial)}.

atom_type MUST be one of: insight, anecdote, quote, framework, statistic,
story_angle, strategy_angle, strategy, endorsement, critique, collection.

Output ONLY the JSON array, no prose."#;

/// Options for [`run_extract_atoms`]. Mirrors TS `ExtractAtomsOpts`.
#[derive(Debug, Clone, Default)]
pub struct ExtractAtomsOpts {
    pub dry_run: bool,
    pub source_id: Option<String>,
    pub brain_dir: Option<String>,
}

/// Result of a single `extract_atoms` run. Mirrors TS `PhaseResult.details`.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractAtomsResult {
    pub atoms_extracted: u64,
    pub transcripts_processed: u64,
    pub transcripts_total: u64,
    pub transcripts_skipped_budget: u64,
    pub pages_processed: u64,
    pub pages_total: u64,
    pub pages_skipped_budget: u64,
    pub duplicates_skipped: u64,
    pub failures: Vec<ExtractionFailure>,
    pub estimated_spend_usd: f64,
    pub budget_usd: f64,
    pub source_id: String,
    pub dry_run: bool,
}

/// A single failed extraction work-item.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionFailure {
    pub source: String,
    pub error: String,
}

/// A single parsed atom from the LLM response.
struct ExtractedAtom {
    title: String,
    atom_type: String,
    body: String,
    source_quote: Option<String>,
    lesson: Option<String>,
    virality_score: Option<f64>,
    emotional_register: Option<String>,
}

/// A merge unit: either a transcript (transcript path not ported → never
/// constructed in 1-1-2) or a brain page.
enum WorkItem {
    Transcript {
        content_hash: String,
        content: String,
    },
    Page {
        slug: String,
        content: String,
        content_hash: String,
    },
}

/// Parse the Haiku JSON response into [`ExtractedAtom`]s. Tolerant of
/// common LLM mistakes: extra prose around the JSON, missing fields,
/// invalid `atom_type` values. Rejects (returns empty) on hard parse fail.
/// Mirrors TS `parseAtomsResponse`.
pub(crate) fn parse_atoms_response(raw: &str) -> Vec<ExtractedAtom> {
    // Strip markdown code fences if the LLM wrapped JSON in them.
    let mut cleaned = raw.trim().to_string();
    if let Some(caps) = cleaned.clone().strip_prefix("```") {
        // crude fence unwrap: take content after optional `json` tag up to ```
        let inner = caps.trim_start_matches("json").trim_start();
        if let Some(end) = inner.find("```") {
            cleaned = inner[..end].trim().to_string();
        }
    }

    // Find the first JSON array bracket.
    let array_start = cleaned.find('[').unwrap_or(usize::MAX);
    if array_start == usize::MAX {
        return Vec::new();
    }
    cleaned = cleaned[array_start..].to_string();

    let parsed: serde_json::Value = match serde_json::from_str::<serde_json::Value>(&cleaned) {
        Ok(v) => v,
        Err(_) => {
            // Try trimming back from the end to recover from trailing prose.
            let array_end = cleaned.rfind(']').unwrap_or(0);
            if array_end == 0 {
                return Vec::new();
            }
            match serde_json::from_str::<serde_json::Value>(&cleaned[..array_end + 1]) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            }
        }
    };

    let arr = match parsed {
        serde_json::Value::Array(a) => a,
        _ => return Vec::new(),
    };

    let mut atoms = Vec::new();
    for item in &arr {
        let obj = match item {
            serde_json::Value::Object(o) => o,
            _ => continue,
        };
        let title = match obj.get("title").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let atom_type = match obj.get("atom_type").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let body = match obj.get("body").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !ATOM_TYPES.contains(&atom_type.as_str()) {
            continue;
        }
        let source_quote = obj
            .get("source_quote")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(500).collect::<String>());
        let lesson = obj.get("lesson").and_then(|v| v.as_str()).map(str::to_string);
        let virality_score = obj.get("virality_score").and_then(|v| v.as_f64());
        let emotional_register = obj
            .get("emotional_register")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        atoms.push(ExtractedAtom {
            title: title.chars().take(200).collect(),
            atom_type,
            body,
            source_quote,
            lesson,
            virality_score,
            emotional_register,
        });
    }
    atoms
}

/// URL/path-safe slug fragment. Mirrors TS `slugify`.
fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
        .collect::<String>()
        .trim()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .replace("--", "-")
        .chars()
        .take(60)
        .collect()
}

/// Run the extract-atoms phase.
///
/// - Discovers extractable brain pages via [`BrainEngine::discover_extractable_pages`]
///   (fail-soft: discovery errors degrade to no pages, so inmemory/postgres
///   without an impl yield `pages_total == 0`).
/// - The transcript path is intentionally absent (KNOWN-GAP G62).
/// - Per work-item, calls `chat` (Haiku) for 1-3 atoms, then writes each as
///   a `type: "atom"` page keyed by `atoms/{date}/{slug}`.
/// - Enforces a per-source Haiku budget (`DEFAULT_BUDGET_USD`); over-budget
///   items are skipped and surfaced as `pages_skipped_budget`.
pub async fn run_extract_atoms(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    opts: &ExtractAtomsOpts,
) -> ZbResult<ExtractAtomsResult> {
    let source_id = opts
        .source_id
        .clone()
        .unwrap_or_else(|| "default".to_string());

    // 1b. Discover pages (fail-soft — a discovery error must not abort the phase).
    let pages: Vec<DiscoveredPage> = match engine.discover_extractable_pages(&source_id, None).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[extract_atoms] page discovery failed: {e}");
            Vec::new()
        }
    };

    // 1a. Transcripts path NOT ported — always empty.
    // registered in docs/plans/MIGRATION.md (G62).
    let transcripts: Vec<DiscoveredPage> = Vec::new();

    // 2-3. Dual-source merge: transcripts + pages, dedup by content_hash.
    //    (transcripts empty in 1-1-2, but keep the merge for fidelity.)
    let mut seen_hashes = std::collections::HashSet::new();
    let mut work: Vec<WorkItem> = Vec::new();
    let mut duplicates_skipped = 0u64;
    for t in &transcripts {
        if seen_hashes.contains(&t.content_hash) {
            duplicates_skipped += 1;
            continue;
        }
        seen_hashes.insert(t.content_hash.clone());
        work.push(WorkItem::Transcript {
            content_hash: t.content_hash.clone(),
            content: t.content.clone(),
        });
    }
    for p in &pages {
        if seen_hashes.contains(&p.content_hash) {
            duplicates_skipped += 1;
            continue;
        }
        seen_hashes.insert(p.content_hash.clone());
        work.push(WorkItem::Page {
            slug: p.slug.clone(),
            content: p.content.clone(),
            content_hash: p.content_hash.clone(),
        });
    }

    let mut result = ExtractAtomsResult {
        atoms_extracted: 0,
        transcripts_processed: 0,
        transcripts_total: transcripts.len() as u64,
        transcripts_skipped_budget: 0,
        pages_processed: 0,
        pages_total: pages.len() as u64,
        pages_skipped_budget: 0,
        duplicates_skipped,
        failures: Vec::new(),
        estimated_spend_usd: 0.0,
        budget_usd: DEFAULT_BUDGET_USD,
        source_id: source_id.clone(),
        dry_run: opts.dry_run,
    };

    for item in &work {
        if result.estimated_spend_usd >= DEFAULT_BUDGET_USD {
            match item {
                WorkItem::Transcript { .. } => result.transcripts_skipped_budget += 1,
                WorkItem::Page { .. } => result.pages_skipped_budget += 1,
            }
            continue;
        }

        let (origin_label, content, content_hash, source_slug_opt) = match item {
            WorkItem::Transcript {
                content_hash,
                content,
            } => (content_hash.clone(), content.clone(), content_hash.clone(), None),
            WorkItem::Page {
                slug,
                content,
                content_hash,
            } => (slug.clone(), content.clone(), content_hash.clone(), Some(slug.clone())),
        };

        let chat_result = match chat
            .chat(ChatOpts {
                model: None,
                system: Some(EXTRACT_PROMPT.to_string()),
                messages: vec![ChatMessage::text(
                    ChatRole::User,
                    format!(
                        "Source: {origin_label}\n\n---\n\n{}",
                        content.chars().take(50_000).collect::<String>()
                    ),
                )],
                tools: vec![],
                max_tokens: Some(2000),
                cache_system: false,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                result.failures.push(ExtractionFailure {
                    source: origin_label,
                    error: e.to_string(),
                });
                continue;
            }
        };

        let usage = &chat_result.usage;
        result.estimated_spend_usd +=
            (usage.input_tokens as f64 * 0.8 + usage.output_tokens as f64 * 4.0) / 1_000_000.0;

        let atoms = parse_atoms_response(&chat_result.text);
        if atoms.is_empty() {
            match item {
                WorkItem::Transcript { .. } => result.transcripts_processed += 1,
                WorkItem::Page { .. } => result.pages_processed += 1,
            }
            continue;
        }

        if !opts.dry_run {
            for atom in &atoms {
                let slug = format!("atoms/{}/{}", today_utc_date(), slugify(&atom.title));
                let source_hash_16: String = content_hash.chars().take(16).collect();
                let mut fm = json!({
                    "type": "atom",
                    "atom_type": atom.atom_type,
                    "source_hash": source_hash_16,
                    "extracted_at": chrono::Utc::now().to_rfc3339(),
                    "extracted_by": "extract_atoms-v0.41.2.1",
                });
                if let Some(ref sq) = atom.source_quote {
                    fm["source_quote"] = json!(sq);
                }
                if let Some(ref le) = atom.lesson {
                    fm["lesson"] = json!(le);
                }
                if let Some(vs) = atom.virality_score {
                    fm["virality_score"] = json!(vs);
                }
                if let Some(ref er) = atom.emotional_register {
                    fm["emotional_register"] = json!(er);
                }
                if let Some(ref slug_src) = source_slug_opt {
                    fm["source_slug"] = json!(slug_src);
                }

                let input = PageInput {
                    page_type: PageType::from("atom"),
                    title: atom.title.clone(),
                    compiled_truth: atom.body.clone(),
                    timeline: None,
                    frontmatter: Some(fm),
                    ..Default::default()
                };

                if let Err(e) = engine.put_page(&slug, Some(&source_id), &input).await {
                    result.failures.push(ExtractionFailure {
                        source: origin_label.clone(),
                        error: e.to_string(),
                    });
                }
                result.atoms_extracted += 1;
            }
        } else {
            result.atoms_extracted += atoms.len() as u64;
        }

        match item {
            WorkItem::Transcript { .. } => result.transcripts_processed += 1,
            WorkItem::Page { .. } => result.pages_processed += 1,
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatError, ChatResult, ChatUsage, StopReason};
    use crate::engine::InMemoryEngine;
    use async_trait::async_trait;

    #[test]
    fn parse_atoms_response_variants() {
        // clean JSON array
        let clean = r#"[{"title":"A","atom_type":"insight","body":"b"}]"#;
        let a = parse_atoms_response(clean);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].atom_type, "insight");

        // fenced + trailing prose
        let fenced = "```json\n[{\"title\":\"B\",\"atom_type\":\"quote\",\"body\":\"x\"}]\n```\nsome note";
        let b = parse_atoms_response(fenced);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].title, "B");

        // invalid atom_type filtered, missing body filtered
        let mixed = r#"[{"title":"C","atom_type":"bogus","body":"y"},{"title":"D","atom_type":"statistic"}]"#;
        let c = parse_atoms_response(mixed);
        assert_eq!(c.len(), 0);

        // not JSON at all
        assert_eq!(parse_atoms_response("no atoms here").len(), 0);
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Multiple   Spaces  "), "multiple-spaces");
        assert_eq!(slugify("keep-dash-and-123"), "keep-dash-and-123");
    }

    #[derive(Debug)]
    struct StubChat {
        called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::ai::chat::ChatProvider for StubChat {
        async fn chat(
            &self,
            _opts: crate::ai::chat::ChatOpts,
        ) -> std::result::Result<ChatResult, ChatError> {
            self.called
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ChatResult {
                text: r#"[{"title":"stub","atom_type":"insight","body":"b"}]"#.to_string(),
                blocks: vec![],
                stop_reason: StopReason::End,
                usage: ChatUsage::default(),
                model: "stub".to_string(),
                provider_id: "stub".to_string(),
                provider_metadata: None,
            })
        }
    }

    #[tokio::test]
    async fn run_extract_atoms_no_work_inmemory() {
        let engine = InMemoryEngine::new();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let chat = StubChat {
            called: called.clone(),
        };
        let result = run_extract_atoms(
            &engine,
            &chat,
            &ExtractAtomsOpts {
                source_id: Some("default".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // inmemory discover returns Err(Unsupported) → fail-soft empty → no work.
        assert_eq!(result.pages_total, 0);
        assert_eq!(result.atoms_extracted, 0);
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
