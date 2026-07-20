//! routing_eval — Check 5 of the skillify checklist (Rust port).
//!
//! Validates that, given a user intent, the skill-resolver table routes to
//! the correct skill. Two layers (per the essay's "both layers matter"
//! framing):
//!
//!   Layer A (structural): always runs, no LLM. Normalize both the intent
//!     and each resolver trigger phrase, then check if any trigger is a
//!     substring of the intent. A fixture `expected_skill` passes iff:
//!       - that skill's trigger matches AND
//!       - no other skill's trigger matches (unambiguous)
//!     Supports negative cases (`expected_skill: null` — nothing should
//!     match) and ambiguity declarations (`ambiguous_with: [...]` — list
//!     of skills this intent is allowed to also match).
//!
//!   Layer B (LLM tie-break): NOT implemented in this release. The TS line
//!     accepted `--llm` as a forward-compat placeholder; the Rust CLI
//!     instead refuses it (exit 1, see roadmap decision 1-6-5-6-6) for
//!     honesty — we don't accept flags that do nothing.
//!
//! Ported from `src/core/routing-eval.ts`. Slice 1-6-5-6 breakdown:
//!   1-6-5-6-1  primitives + indexResolverTriggers        (this file, section A)
//!   1-6-5-6-2  lintRoutingFixtures                        (section B)
//!   1-6-5-6-3  loadRoutingFixtures                        (section C)
//!   1-6-5-6-4  runRoutingEval + structuralRouteMatch      (section D)
//!   1-6-5-6-5  wire Check 5 into check_resolvable()       (check_resolvable.rs)

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::skill_resolver::trigger_index::parse_resolver_entries;

// ---------------------------------------------------------------------------
// Shared fixture type (consumed by lint / load / run)
// ---------------------------------------------------------------------------

/// A single routing fixture: a natural-language user intent and the skill
/// slug that should fire. Mirrors `src/core/routing-eval.ts::RoutingFixture`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingFixture {
    /// Natural-language user intent. Required.
    pub intent: String,
    /// Skill slug (matches the directory name under `skills/`) that should
    /// fire. `None` for negative cases: "nothing should match this intent."
    #[serde(rename = "expected_skill")]
    pub expected_skill: Option<String>,
    /// Skills the intent is ALLOWED to also match without being flagged as
    /// ambiguous (always-on co-fire skills like signal-detector).
    #[serde(rename = "ambiguous_with")]
    pub ambiguous_with: Option<Vec<String>>,
    /// Source path this fixture came from (populated by the loader).
    #[serde(default)]
    pub source: Option<String>,
}

// ---------------------------------------------------------------------------
// Section A — normalization + trigger extraction + resolver→index
// (slice 1-6-5-6-1)
// ---------------------------------------------------------------------------

/// Normalize a string for routing comparison:
///   - lowercase
///   - replace any run of non-alphanumeric chars with a single space
///   - trim
///
/// Stripping punctuation is deliberately aggressive. Question marks,
/// quotes, dashes, commas, and apostrophes all collapse to spaces. This
/// means `"What's up?"` and `whats up` compare equal — which is what a
/// routing match should do.
fn normalize_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap())
}

pub fn normalize_text(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let lower = s.to_lowercase();
    normalize_re().replace_all(&lower, " ").trim().to_string()
}

/// Extract candidate trigger phrases from a resolver cell. Two shapes:
///   1. Cell contains double-quoted strings → return each quoted phrase
///      separately, normalized.
///   2. Cell has no quotes → return [whole cell] as one normalized phrase.
/// Phrases shorter than 3 normalized chars are dropped (empty/trivial).
fn quoted_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""([^"]+)""#).unwrap())
}

pub fn extract_trigger_phrases(cell_text: &str) -> Vec<String> {
    let quoted: Vec<String> = quoted_re()
        .captures_iter(cell_text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();
    let source: Vec<String> = if !quoted.is_empty() {
        quoted
    } else {
        vec![cell_text.to_string()]
    };
    source
        .into_iter()
        .map(|s| normalize_text(&s))
        .filter(|s| s.len() >= 3)
        .collect()
}

/// Skill slug extracted from a resolver skillPath like `skills/foo/SKILL.md`
/// → `foo`. Returns None for paths that don't match the canonical shape
/// (e.g. `skills/foo/bar/SKILL.md` — nested SKILL.md under a skill dir).
fn slug_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^skills/([^/]+)/SKILL\.md$").unwrap())
}

fn skill_slug_from_path(skill_path: &str) -> Option<String> {
    slug_re()
        .captures(skill_path)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// Map of skill slug → set of normalized trigger phrases.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillTriggerIndex {
    pub skill_phrases: HashMap<String, Vec<String>>,
}

/// Build the skill→phrases index from synthesized resolver content
/// (as produced by `trigger_index::entries_to_resolver_content`). GStack
/// rows are skipped (they carry no concrete skill trigger).
pub fn index_resolver_triggers(resolver_content: &str) -> SkillTriggerIndex {
    let entries = parse_resolver_entries(resolver_content);
    let mut skill_phrases: HashMap<String, Vec<String>> = HashMap::new();
    for e in entries {
        if e.is_gstack {
            continue;
        }
        let slug = match skill_slug_from_path(&e.skill_path) {
            Some(s) => s,
            None => continue,
        };
        let phrases = extract_trigger_phrases(&e.trigger);
        skill_phrases.entry(slug).or_default().extend(phrases);
    }
    SkillTriggerIndex { skill_phrases }
}

// ---------------------------------------------------------------------------
// Section B — fixture linter (slice 1-6-5-6-2)
// ---------------------------------------------------------------------------

/// Why a fixture was rejected by the linter (D-CX-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureLintReason {
    /// Normalized intent is verbatim-identical to one of its expected
    /// skill's trigger phrases — a copy-paste tautology.
    IntentCopiesTrigger,
    /// `expected_skill` is not present in the resolver index.
    UnknownExpectedSkill,
    /// Shape violation (e.g. empty intent).
    InvalidShape,
}

impl FixtureLintReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            FixtureLintReason::IntentCopiesTrigger => "intent_copies_trigger",
            FixtureLintReason::UnknownExpectedSkill => "unknown_expected_skill",
            FixtureLintReason::InvalidShape => "invalid_shape",
        }
    }
}

/// A fixture rejected by `lint_routing_fixtures`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixtureLintIssue {
    pub fixture: RoutingFixture,
    pub reason: FixtureLintReason,
    pub detail: String,
}

/// Lint fixtures against the resolver (D-CX-6). Reject cases where:
///   - The normalized intent EQUALS any trigger phrase for its expected
///     skill (pure tautology — the fixture IS the trigger).
///   - The expected_skill is unknown to the resolver.
///
/// We deliberately do NOT reject intents that merely CONTAIN trigger words
/// in a natural sentence (Layer A's whole mechanism is substring match on
/// resolver triggers; a fixture embedding trigger words in context is valid
/// and useful). The linter catches copy-paste tautologies, not word overlap.
pub fn lint_routing_fixtures(
    fixtures: &[RoutingFixture],
    index: &SkillTriggerIndex,
) -> Vec<FixtureLintIssue> {
    let mut issues = Vec::new();
    for f in fixtures {
        if f.intent.trim().is_empty() {
            issues.push(FixtureLintIssue {
                fixture: f.clone(),
                reason: FixtureLintReason::InvalidShape,
                detail: "intent must be a non-empty string".to_string(),
            });
            continue;
        }
        // Negative case (None) can't copy a trigger — skip that check.
        let expected = match &f.expected_skill {
            Some(e) => e,
            None => continue,
        };
        if !index.skill_phrases.contains_key(expected) {
            issues.push(FixtureLintIssue {
                fixture: f.clone(),
                reason: FixtureLintReason::UnknownExpectedSkill,
                detail: format!("expected_skill '{}' is not in the resolver", expected),
            });
            continue;
        }
        let normalized_intent = normalize_text(&f.intent);
        let phrases = index.skill_phrases.get(expected).unwrap();
        for phrase in phrases {
            if !phrase.is_empty() && normalized_intent == *phrase {
                issues.push(FixtureLintIssue {
                    fixture: f.clone(),
                    reason: FixtureLintReason::IntentCopiesTrigger,
                    detail: format!("intent is verbatim-identical to trigger phrase '{}'", phrase),
                });
                break;
            }
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// Section C — fixture loader (slice 1-6-5-6-3)
// ---------------------------------------------------------------------------

/// A malformed JSONL line that failed to parse (or was missing `intent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MalformedFixture {
    pub file: String,
    pub line: usize,
    pub raw: String,
    pub error: String,
}

/// Result of scanning the skills tree for `routing-eval.jsonl` fixtures.
#[derive(Debug, Clone, Default)]
pub struct LoadResult {
    pub fixtures: Vec<RoutingFixture>,
    pub malformed: Vec<MalformedFixture>,
}

/// Walk each child of `skills_dir` looking for `routing-eval.jsonl` and
/// return all fixtures with the source path attached. JSONL format: one
/// JSON object per non-empty line; lines starting with `//` or `#` are
/// skipped as comments. Malformed lines are returned via `malformed[]`.
pub fn load_routing_fixtures(skills_dir: &Path) -> LoadResult {
    let mut result = LoadResult::default();
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let fixture_path = entry.path().join("routing-eval.jsonl");
        if !fixture_path.exists() {
            continue;
        }
        let content = match std::fs::read_to_string(&fixture_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for (i, raw_line) in content.split('\n').enumerate() {
            let raw = raw_line.trim();
            if raw.is_empty() {
                continue;
            }
            if raw.starts_with("//") || raw.starts_with('#') {
                continue;
            }
            match serde_json::from_str::<RoutingFixture>(raw) {
                Ok(mut obj) => {
                    if obj.intent.trim().is_empty() {
                        result.malformed.push(MalformedFixture {
                            file: fixture_path.display().to_string(),
                            line: i + 1,
                            raw: raw.to_string(),
                            error: format!(
                                "missing required field 'intent' (found keys: {})",
                                obj_keys(&obj)
                            ),
                        });
                        continue;
                    }
                    obj.source = Some(fixture_path.display().to_string());
                    result.fixtures.push(obj);
                }
                Err(e) => {
                    result.malformed.push(MalformedFixture {
                        file: fixture_path.display().to_string(),
                        line: i + 1,
                        raw: raw.to_string(),
                        error: e.to_string(),
                    });
                }
            }
        }
    }
    result
}

/// Best-effort key list for a malformed-fixture error message. Mirrors the
/// TS `Object.keys(obj).join(', ')` used when intent is absent after parse.
fn obj_keys(f: &RoutingFixture) -> String {
    let mut keys = vec!["intent".to_string()];
    if f.expected_skill.is_some() {
        keys.push("expected_skill".to_string());
    }
    if f.ambiguous_with.is_some() {
        keys.push("ambiguous_with".to_string());
    }
    if f.source.is_some() {
        keys.push("source".to_string());
    }
    keys.join(", ")
}

// ---------------------------------------------------------------------------
// Section D — eval runner (slice 1-6-5-6-4)
// ---------------------------------------------------------------------------

/// Skills that routinely co-fire with any target skill. A match that
/// includes one of these alongside a specific target is NOT considered
/// ambiguous (Layer A substring match naturally catches them).
pub const ALWAYS_ON_SKILLS: &[&str] = &["signal-detector", "brain-ops", "ingest"];

/// Outcome of evaluating a single fixture against the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingOutcome {
    /// `expected_skill` matched and nothing unexpected also matched.
    Pass,
    /// `expected_skill` was not in the match set.
    Missed,
    /// Matched `expected_skill` AND other skills not listed in
    /// `ambiguous_with` (or always-on).
    Ambiguous,
    /// Negative case (`expected_skill: null`) matched a specific skill.
    FalsePositive,
}

impl RoutingOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoutingOutcome::Pass => "pass",
            RoutingOutcome::Missed => "missed",
            RoutingOutcome::Ambiguous => "ambiguous",
            RoutingOutcome::FalsePositive => "false_positive",
        }
    }
}

/// Result of the structural (Layer A) match for one intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralMatchResult {
    /// Skills whose trigger phrases are substrings of the normalized intent,
    /// sorted for deterministic reporting.
    pub matched: Vec<String>,
    /// True if more than one non-always-on skill matched.
    pub ambiguous: bool,
}

/// Layer A (structural) routing match: no LLM, substring match of each
/// resolver trigger phrase against the normalized intent.
pub fn structural_route_match(intent: &str, index: &SkillTriggerIndex) -> StructuralMatchResult {
    let normalized_intent = normalize_text(intent);
    let mut matched: Vec<String> = Vec::new();
    for (slug, phrases) in &index.skill_phrases {
        let mut hit = false;
        for phrase in phrases {
            if !phrase.is_empty() && normalized_intent.contains(phrase) {
                hit = true;
                break;
            }
        }
        if hit {
            matched.push(slug.clone());
        }
    }
    // Deterministic order for notes / downstream equality.
    matched.sort();
    let specific_count = matched
        .iter()
        .filter(|s| !ALWAYS_ON_SKILLS.contains(&s.as_str()))
        .count();
    StructuralMatchResult {
        matched,
        ambiguous: specific_count > 1,
    }
}

/// One evaluated fixture with its outcome and diagnostic note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingCaseResult {
    pub fixture: RoutingFixture,
    pub outcome: RoutingOutcome,
    pub matched_skills: Vec<String>,
    pub note: Option<String>,
}

/// Aggregate report over a fixture set.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingReport {
    pub total_cases: usize,
    pub top1_accuracy: f64,
    pub passed: usize,
    pub missed: usize,
    pub ambiguous: usize,
    pub false_positives: usize,
    pub details: Vec<RoutingCaseResult>,
}

impl RoutingReport {
    /// Outcome of the i-th fixture (panics if out of bounds — tests only).
    pub fn outcome_of(&self, i: usize) -> RoutingOutcome {
        self.details[i].outcome
    }
}

/// Run the structural (Layer A) routing eval against a resolver table and a
/// fixture set. Mirrors `src/core/routing-eval.ts::runRoutingEval` exactly:
/// builds the index, classifies each fixture into pass/missed/ambiguous/
/// false_positive, and tallies counts + top-1 accuracy.
///
/// Layer B (LLM tie-break) is intentionally NOT implemented here — the Rust
/// CLI refuses `--llm` (see roadmap decision 1-6-5-6-6) rather than silently
/// ignoring it.
pub fn run_routing_eval(resolver_content: &str, fixtures: &[RoutingFixture]) -> RoutingReport {
    let index = index_resolver_triggers(resolver_content);
    let mut details: Vec<RoutingCaseResult> = Vec::new();
    let mut passed = 0usize;
    let mut missed = 0usize;
    let mut ambiguous = 0usize;
    let mut false_positives = 0usize;

    for fixture in fixtures {
        let result = structural_route_match(&fixture.intent, &index);
        let (outcome, note) = if fixture.expected_skill.is_none() {
            // Negative case: nothing SPECIFIC should match.
            let specific: Vec<&String> = result
                .matched
                .iter()
                .filter(|s| !ALWAYS_ON_SKILLS.contains(&s.as_str()))
                .collect();
            if specific.is_empty() {
                passed += 1;
                (RoutingOutcome::Pass, None)
            } else {
                false_positives += 1;
                (
                    RoutingOutcome::FalsePositive,
                    Some(format!(
                        "negative case unexpectedly matched: {}",
                        specific
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                )
            }
        } else if !result.matched.contains(fixture.expected_skill.as_ref().unwrap()) {
            missed += 1;
            let note = if result.matched.is_empty() {
                "no matches".to_string()
            } else {
                format!("matched instead: {}", result.matched.join(", "))
            };
            (RoutingOutcome::Missed, Some(note))
        } else {
            // expected_skill matched; check for ambiguity beyond allow-list.
            let expected = fixture.expected_skill.as_ref().unwrap();
            let mut allowed: HashSet<String> = HashSet::new();
            if let Some(aw) = &fixture.ambiguous_with {
                for s in aw {
                    allowed.insert(s.clone());
                }
            }
            for s in ALWAYS_ON_SKILLS {
                allowed.insert((*s).to_string());
            }
            allowed.insert(expected.clone());
            let unexpected: Vec<String> = result
                .matched
                .iter()
                .filter(|s| !allowed.contains(*s))
                .cloned()
                .collect();
            if unexpected.is_empty() {
                passed += 1;
                (RoutingOutcome::Pass, None)
            } else {
                ambiguous += 1;
                (
                    RoutingOutcome::Ambiguous,
                    Some(format!("also matched: {}", unexpected.join(", "))),
                )
            }
        };
        details.push(RoutingCaseResult {
            fixture: fixture.clone(),
            outcome,
            matched_skills: result.matched.clone(),
            note,
        });
    }

    let total_cases = fixtures.len();
    let top1_accuracy = if total_cases == 0 {
        1.0
    } else {
        passed as f64 / total_cases as f64
    };
    RoutingReport {
        total_cases,
        top1_accuracy,
        passed,
        missed,
        ambiguous,
        false_positives,
        details,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_punctuation_and_lowercases() {
        assert_eq!(normalize_text("What's up?"), "what s up");
        assert_eq!(normalize_text("Creating/enriching a person or company page"),
            "creating enriching a person or company page");
        assert_eq!(normalize_text("  Hello,   WORLD!  "), "hello world");
    }

    #[test]
    fn normalize_empty_returns_empty() {
        assert_eq!(normalize_text(""), "");
        assert_eq!(normalize_text("!!!"), "");
    }

    #[test]
    fn extract_quoted_phrases_each_normalized() {
        let got = extract_trigger_phrases(r#""what do we know about", "tell me about""#);
        assert_eq!(got, vec!["what do we know about", "tell me about"]);
    }

    #[test]
    fn extract_unquoted_cell_is_one_phrase() {
        let got = extract_trigger_phrases("Creating/enriching a person or company page");
        assert_eq!(got, vec!["creating enriching a person or company page"]);
    }

    #[test]
    fn extract_drops_trivially_short_phrases() {
        // "up" normalizes to len 2 (< 3) → dropped; "go" too.
        let got = extract_trigger_phrases(r#""up", "look up", "go""#);
        assert_eq!(got, vec!["look up"]);
    }

    #[test]
    fn slug_from_canonical_path() {
        assert_eq!(skill_slug_from_path("skills/foo/SKILL.md"), Some("foo".to_string()));
    }

    #[test]
    fn slug_rejects_nested_skill_path() {
        assert_eq!(skill_slug_from_path("skills/foo/bar/SKILL.md"), None);
        assert_eq!(skill_slug_from_path("skills/foo/README.md"), None);
    }

    #[test]
    fn index_builds_skill_to_phrases_map() {
        // Real RESOLVER.md uses the markdown-TABLE format with double-quoted
        // trigger phrases; parse_resolver_entries keeps the quotes and
        // extract_trigger_phrases splits them. (Compact-list backtick form
        // is intentionally NOT supported here — it matches TS parity:
        // routing-eval only extracts phrases from the table format.)
        let resolver = "\
| Trigger | Skill |
|---------|-------|
| \"what do we know about\", \"tell me about\" | `skills/query/SKILL.md` |
| \"summarize this\" | `skills/summarize/SKILL.md` |
";
        let idx = index_resolver_triggers(resolver);
        assert_eq!(idx.skill_phrases.get("query"),
            Some(&vec!["what do we know about".to_string(), "tell me about".to_string()]));
        assert_eq!(idx.skill_phrases.get("summarize"),
            Some(&vec!["summarize this".to_string()]));
    }

    #[test]
    fn index_skips_gstack_rows() {
        let resolver = "\
| Trigger | Skill |
|---------|-------|
| Check something | GStack: gstack/whatever |
";
        let idx = index_resolver_triggers(resolver);
        assert!(idx.skill_phrases.is_empty());
    }

    // -----------------------------------------------------------------------
    // Section B — lint (1-6-5-6-2)
    // -----------------------------------------------------------------------

    fn index_with(extra: &str) -> SkillTriggerIndex {
        let resolver = format!(
            "\
| Trigger | Skill |
|---------|-------|
| \"what do we know about\", \"tell me about\" | `skills/query/SKILL.md` |
| \"summarize this\" | `skills/summarize/SKILL.md` |
{}
",
            extra
        );
        index_resolver_triggers(&resolver)
    }

    #[test]
    fn lint_rejects_empty_intent() {
        let idx = index_with("");
        let fixtures = vec![RoutingFixture {
            intent: "   ".to_string(),
            expected_skill: Some("query".to_string()),
            ambiguous_with: None,
            source: None,
        }];
        let issues = lint_routing_fixtures(&fixtures, &idx);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].reason, FixtureLintReason::InvalidShape);
    }

    #[test]
    fn lint_rejects_unknown_expected_skill() {
        let idx = index_with("");
        let fixtures = vec![RoutingFixture {
            intent: "real natural language request".to_string(),
            expected_skill: Some("nonexistent".to_string()),
            ambiguous_with: None,
            source: None,
        }];
        let issues = lint_routing_fixtures(&fixtures, &idx);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].reason, FixtureLintReason::UnknownExpectedSkill);
    }

    #[test]
    fn lint_rejects_intent_copies_trigger() {
        let idx = index_with("");
        let fixtures = vec![RoutingFixture {
            intent: "what do we know about".to_string(),
            expected_skill: Some("query".to_string()),
            ambiguous_with: None,
            source: None,
        }];
        let issues = lint_routing_fixtures(&fixtures, &idx);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].reason, FixtureLintReason::IntentCopiesTrigger);
    }

    #[test]
    fn lint_allows_paraphrase_with_trigger_words() {
        // Intent embeds trigger words in context but is NOT verbatim-equal
        // to the phrase — Layer A substring match on surrounding context is
        // valid, so the linter must pass it.
        let idx = index_with("");
        let fixtures = vec![RoutingFixture {
            intent: "please summarize this document for me".to_string(),
            expected_skill: Some("summarize".to_string()),
            ambiguous_with: None,
            source: None,
        }];
        assert!(lint_routing_fixtures(&fixtures, &idx).is_empty());
    }

    #[test]
    fn lint_skips_negative_cases() {
        let idx = index_with("");
        let fixtures = vec![RoutingFixture {
            intent: "what do we know about".to_string(), // would be tautology if expected set
            expected_skill: None,
            ambiguous_with: None,
            source: None,
        }];
        // Negative case can't copy a trigger; no issue.
        assert!(lint_routing_fixtures(&fixtures, &idx).is_empty());
    }

    // -----------------------------------------------------------------------
    // Section C — loader (1-6-5-6-3)
    // -----------------------------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};
    static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let n = SCRATCH_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("zbrain_reval_{}_{}", tag, n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn load_finds_fixtures_and_skips_comments() {
        let root = scratch_dir("find");
        let skill = root.join("query");
        std::fs::create_dir_all(&skill).unwrap();
        let jsonl = "\
// this is a comment
# also a comment
{\"intent\":\"look up the paper\",\"expected_skill\":\"query\"}
{\"intent\":\"tell me about the project\",\"expected_skill\":\"query\"}
{bad json line
";
        std::fs::write(skill.join("routing-eval.jsonl"), jsonl).unwrap();

        let result = load_routing_fixtures(&root);
        assert_eq!(result.fixtures.len(), 2);
        for f in &result.fixtures {
            assert_eq!(f.source.as_deref(), Some(skill.join("routing-eval.jsonl").to_str().unwrap()));
        }
        assert_eq!(result.malformed.len(), 1);
        assert!(result.malformed[0].raw.contains("bad json"));
    }

    #[test]
    fn load_skips_dot_and_underscore_dirs() {
        let root = scratch_dir("dot");
        for hidden in [".hidden", "_private"] {
            let d = root.join(hidden);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("routing-eval.jsonl"),
                "{\"intent\":\"x\",\"expected_skill\":\"query\"}",
            )
            .unwrap();
        }
        let result = load_routing_fixtures(&root);
        assert!(result.fixtures.is_empty());
        assert!(result.malformed.is_empty());
    }

    #[test]
    fn load_missing_skills_dir_returns_empty() {
        let root = scratch_dir("missing").join("does-not-exist");
        let result = load_routing_fixtures(&root);
        assert!(result.fixtures.is_empty());
        assert!(result.malformed.is_empty());
    }

    #[test]
    fn load_malformed_missing_intent_reports_keys() {
        let root = scratch_dir("malformed");
        let skill = root.join("query");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("routing-eval.jsonl"),
            "{\"expected_skill\":\"query\"}",
        )
        .unwrap();
        let result = load_routing_fixtures(&root);
        assert_eq!(result.fixtures.len(), 0);
        assert_eq!(result.malformed.len(), 1);
        assert!(result.malformed[0].error.contains("intent"));
    }

    // -----------------------------------------------------------------------
    // Section D — runner (1-6-5-6-4)
    // -----------------------------------------------------------------------

    fn rich_index_resolver() -> String {
        "\
| Trigger | Skill |
|---------|-------|
| \"what do we know about\" | `skills/query/SKILL.md` |
| \"detect a signal\" | `skills/signal-detector/SKILL.md` |
| \"look up\" | `skills/search/SKILL.md` |
| \"summarize this\" | `skills/summarize/SKILL.md` |
"
        .to_string()
    }

    fn fx(intent: &str, expected: Option<&str>, ambiguous_with: Option<Vec<&str>>) -> RoutingFixture {
        RoutingFixture {
            intent: intent.to_string(),
            expected_skill: expected.map(|s| s.to_string()),
            ambiguous_with: ambiguous_with.map(|v| v.into_iter().map(|s| s.to_string()).collect()),
            source: None,
        }
    }

    #[test]
    fn run_passes_positive_match() {
        let r = run_routing_eval(&rich_index_resolver(), &[fx("what do we know about the project", Some("query"), None)]);
        assert_eq!(r.passed, 1);
        assert_eq!(r.outcome_of(0), RoutingOutcome::Pass);
        assert_eq!(r.total_cases, 1);
        assert_eq!(r.top1_accuracy, 1.0);
    }

    #[test]
    fn run_missed_when_no_match() {
        let r = run_routing_eval(&rich_index_resolver(), &[fx("zzz totally unrelated", Some("query"), None)]);
        assert_eq!(r.missed, 1);
        assert_eq!(r.outcome_of(0), RoutingOutcome::Missed);
        assert!(r.details[0].note.as_deref().unwrap().contains("no matches"));
    }

    #[test]
    fn run_negative_case_pass() {
        let r = run_routing_eval(&rich_index_resolver(), &[fx("zzz unrelated", None, None)]);
        assert_eq!(r.passed, 1);
        assert_eq!(r.outcome_of(0), RoutingOutcome::Pass);
        assert_eq!(r.false_positives, 0);
    }

    #[test]
    fn run_negative_case_false_positive() {
        let r = run_routing_eval(&rich_index_resolver(), &[fx("look up the thing", None, None)]);
        assert_eq!(r.false_positives, 1);
        assert_eq!(r.outcome_of(0), RoutingOutcome::FalsePositive);
        assert!(r.details[0].note.as_deref().unwrap().contains("unexpectedly matched"));
    }

    #[test]
    fn run_ambiguous_when_two_specific_match() {
        let r = run_routing_eval(
            &rich_index_resolver(),
            &[fx("what do we know about look up", Some("query"), None)],
        );
        assert_eq!(r.ambiguous, 1);
        assert_eq!(r.outcome_of(0), RoutingOutcome::Ambiguous);
        assert!(r.details[0].note.as_deref().unwrap().contains("also matched"));
    }

    #[test]
    fn run_always_on_skill_is_not_ambiguous() {
        // intent matches query AND signal-detector (always-on) → still pass.
        let r = run_routing_eval(
            &rich_index_resolver(),
            &[fx("detect a signal what do we know about", Some("query"), None)],
        );
        assert_eq!(r.passed, 1);
        assert_eq!(r.outcome_of(0), RoutingOutcome::Pass);
    }

    #[test]
    fn run_ambiguous_with_exempts_listed_skill() {
        let r = run_routing_eval(
            &rich_index_resolver(),
            &[fx("what do we know about look up", Some("query"), Some(vec!["search"]))],
        );
        assert_eq!(r.passed, 1);
        assert_eq!(r.outcome_of(0), RoutingOutcome::Pass);
    }

    #[test]
    fn run_report_tallies_all_outcomes() {
        let fixtures = vec![
            fx("what do we know about the project", Some("query"), None), // pass
            fx("zzz unrelated", Some("query"), None),                     // missed
            fx("what do we know about look up", Some("query"), None),     // ambiguous
            fx("look up the thing", None, None),                          // false_positive
            fx("zzz unrelated", None, None),                              // negative pass
        ];
        let r = run_routing_eval(&rich_index_resolver(), &fixtures);
        assert_eq!(r.total_cases, 5);
        assert_eq!(r.passed, 2); // positive + negative
        assert_eq!(r.missed, 1);
        assert_eq!(r.ambiguous, 1);
        assert_eq!(r.false_positives, 1);
        assert!((r.top1_accuracy - 0.4).abs() < 1e-9);
    }
}
