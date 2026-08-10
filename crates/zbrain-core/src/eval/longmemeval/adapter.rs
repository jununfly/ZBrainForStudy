//! LongMemEval haystack -> ZBrain page conversion.
//!
//! Pure data-shape converter. No I/O, no engine, no LLM. Fed by the runner in
//! `eval::longmemeval::runner`, which then calls `import_from_content` on each
//! page in turn.
//!
//! Output slug prefix is `chat/` because the source data is conversation
//! sessions. Page type is `note` (an existing page type); adding a first-class
//! `chat` type would touch the source-boost map and is out of scope. The
//! `chat/` slug prefix does NOT prefix-match any default source-boost entry,
//! so the retrieval factor stays at 1.0.
//!
//! Port of TS `src/eval/longmemeval/adapter.ts` (v0.28.1, `_s`-split support
//! added in v0.35.1.1).

use serde::{Deserialize, Serialize};

/// One conversational turn inside a haystack session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongMemEvalTurn {
    /// `user` or `assistant` in the published dataset. Kept as a free-form
    /// string so an unknown role renders instead of failing the whole run.
    pub role: String,
    pub content: String,
}

/// A haystack session in the "oracle" (structured) dataset shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongMemEvalSession {
    pub session_id: String,
    pub turns: Vec<LongMemEvalTurn>,
}

/// One benchmark question plus its haystack.
///
/// `haystack_sessions` is held as a raw [`serde_json::Value`] because two
/// on-disk shapes are accepted (see [`normalize_sessions`]); a typed enum
/// would reject mixed/corrupt rows that the TS adapter silently skips.
#[derive(Debug, Clone, Deserialize)]
pub struct LongMemEvalQuestion {
    pub question_id: String,
    #[serde(default)]
    pub question_type: String,
    pub question: String,
    #[serde(default)]
    pub answer: String,
    /// Two on-disk shapes are accepted (normalized by [`haystack_to_pages`]):
    ///
    /// 1. Oracle/structured: array of `{session_id, turns}` objects.
    /// 2. `_s` split (the HuggingFace public download): array of turn arrays.
    ///    Session IDs live in a sibling `haystack_session_ids` parallel array.
    #[serde(default)]
    pub haystack_sessions: serde_json::Value,
    /// Parallel to `haystack_sessions` in the `_s` split. Absent in oracle shape.
    #[serde(default)]
    pub haystack_session_ids: Option<Vec<String>>,
    /// ISO date strings, parallel to `haystack_sessions`. Some splits omit this.
    #[serde(default)]
    pub haystack_dates: Option<Vec<String>>,
    /// Ground truth: which haystack sessions actually contain the answer.
    #[serde(default)]
    pub answer_session_ids: Option<Vec<String>>,
}

/// A page ready to hand to `import_from_content`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageInputForImport {
    pub slug: String,
    pub content: String,
}

/// Render one LongMemEval session as a markdown page.
///
/// The body is `**user:** ...\n\n**assistant:** ...\n\n` so retrieval matches
/// naturally on either role's text. Frontmatter pins type, date (when
/// available), and `session_id` so the JSONL emit step can recover the
/// session id from a chunk.
fn render_session(session: &LongMemEvalSession, date: Option<&str>) -> String {
    let mut fm: Vec<String> = vec!["---".to_string(), "type: note".to_string()];
    if let Some(d) = date {
        fm.push(format!("date: {d}"));
    }
    fm.push(format!("session_id: {}", session.session_id));
    fm.push("---".to_string());
    fm.push(String::new());

    let mut body: Vec<String> = Vec::new();
    for turn in &session.turns {
        body.push(format!("**{}:** {}", turn.role, turn.content));
        body.push(String::new());
    }
    // TS: `fm.join('\n') + body.join('\n')` — the trailing empty fm entry
    // supplies the newline after the closing `---`, and there is deliberately
    // NO blank line between frontmatter and the first turn.
    format!("{}{}", fm.join("\n"), body.join("\n"))
}

/// Normalize the on-disk `haystack_sessions` shape (oracle OR `_s`) into the
/// structured `{session_id, turns}` form [`render_session`] consumes.
///
/// The public `_s` split uses an array of turn arrays plus a parallel
/// `haystack_session_ids`. Malformed entries are silently skipped, matching
/// the TS adapter — it keeps a run progressing on mixed/corrupt datasets, and
/// the per-question error boundary in the runner catches whole-question
/// failures anyway.
fn normalize_sessions(question: &LongMemEvalQuestion) -> Vec<LongMemEvalSession> {
    let mut sessions: Vec<LongMemEvalSession> = Vec::new();
    let empty_ids: Vec<String> = Vec::new();
    let ids = question.haystack_session_ids.as_ref().unwrap_or(&empty_ids);
    let Some(raw) = question.haystack_sessions.as_array() else {
        return sessions;
    };
    for (i, item) in raw.iter().enumerate() {
        if let Some(turn_arr) = item.as_array() {
            // `_s` shape: this entry is a turn array directly.
            let sid = ids
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("lme_{}_{}", question.question_id, i));
            let turns: Vec<LongMemEvalTurn> = turn_arr
                .iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect();
            sessions.push(LongMemEvalSession {
                session_id: sid,
                turns,
            });
        } else if let Some(obj) = item.as_object() {
            // Oracle shape: `{session_id, turns}` object.
            let Some(turns_val) = obj.get("turns").and_then(|t| t.as_array()) else {
                continue;
            };
            let sid = obj
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("lme_{}_{}", question.question_id, i));
            let turns: Vec<LongMemEvalTurn> = turns_val
                .iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect();
            sessions.push(LongMemEvalSession {
                session_id: sid,
                turns,
            });
        }
    }
    sessions
}

/// Normalize an arbitrary session id into something the page-slug validator
/// accepts.
///
/// Validator rules: segments are `[a-z0-9CJK-]+`, forward-slash separated. The
/// HuggingFace `_s` split uses `sharegpt_yywfIrx_0`-style ids with underscores
/// AND uppercase letters, both of which are rejected. Lowercase +
/// underscore/dot -> hyphen produces a stable, validator-passing alias.
/// Collisions are negligible per question (each question's slug space is fresh
/// because the runner builds a new benchmark brain per question).
pub fn sanitize_session_id_for_slug(session_id: &str) -> String {
    session_id
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                // Mirrors the TS two-step `[_.] -> '-'` then `[^a-z0-9-] -> '-'`:
                // both steps collapse to the same replacement character.
                '-'
            }
        })
        .collect()
}

/// Convert a question's haystack into importable pages, one per session.
#[must_use]
pub fn haystack_to_pages(question: &LongMemEvalQuestion) -> Vec<PageInputForImport> {
    let empty_dates: Vec<String> = Vec::new();
    let dates = question.haystack_dates.as_ref().unwrap_or(&empty_dates);
    normalize_sessions(question)
        .iter()
        .enumerate()
        .map(|(i, session)| PageInputForImport {
            slug: format!("chat/{}", sanitize_session_id_for_slug(&session.session_id)),
            content: render_session(session, dates.get(i).map(String::as_str)),
        })
        .collect()
}

/// Recover the session id from a `chat/<session_id>` slug.
///
/// Mirrors TS `sessionIdFromSlug` in the command module — kept here next to
/// the slug producer so the two stay in sync.
#[must_use]
pub fn session_id_from_slug(slug: &str) -> &str {
    match slug.find('/') {
        Some(idx) => &slug[idx + 1..],
        None => slug,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(json: serde_json::Value) -> LongMemEvalQuestion {
        serde_json::from_value(json).expect("fixture must deserialize")
    }

    #[test]
    fn oracle_shape_renders_pages() {
        let question = q(serde_json::json!({
            "question_id": "q1",
            "question_type": "temporal-reasoning",
            "question": "when?",
            "answer": "may",
            "haystack_sessions": [
                {"session_id": "S_1", "turns": [
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": "hello"}
                ]}
            ],
            "haystack_dates": ["2026-05-01"],
            "answer_session_ids": ["S_1"]
        }));
        let pages = haystack_to_pages(&question);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].slug, "chat/s-1");
        assert_eq!(
            pages[0].content,
            "---\ntype: note\ndate: 2026-05-01\nsession_id: S_1\n---\n**user:** hi\n\n**assistant:** hello\n"
        );
    }

    #[test]
    fn s_split_shape_uses_parallel_ids() {
        let question = q(serde_json::json!({
            "question_id": "q2",
            "question_type": "multi-session",
            "question": "what?",
            "haystack_sessions": [
                [{"role": "user", "content": "a"}],
                [{"role": "user", "content": "b"}]
            ],
            "haystack_session_ids": ["sharegpt_Yyw_0", "sharegpt_Yyw_1"]
        }));
        let pages = haystack_to_pages(&question);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].slug, "chat/sharegpt-yyw-0");
        assert_eq!(pages[1].slug, "chat/sharegpt-yyw-1");
        // No date entry -> no `date:` frontmatter line.
        assert!(!pages[0].content.contains("date:"));
    }

    #[test]
    fn missing_session_id_falls_back_to_index_name() {
        let question = q(serde_json::json!({
            "question_id": "q3",
            "question": "x",
            "haystack_sessions": [[{"role": "user", "content": "c"}]]
        }));
        let pages = haystack_to_pages(&question);
        assert_eq!(pages[0].slug, "chat/lme-q3-0");
    }

    #[test]
    fn malformed_entries_are_skipped() {
        let question = q(serde_json::json!({
            "question_id": "q4",
            "question": "x",
            "haystack_sessions": [
                42,
                {"session_id": "ok", "turns": [{"role": "user", "content": "y"}]},
                {"no_turns": true}
            ]
        }));
        let pages = haystack_to_pages(&question);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].slug, "chat/ok");
    }

    #[test]
    fn absent_haystack_yields_no_pages() {
        let question = q(serde_json::json!({"question_id": "q5", "question": "x"}));
        assert!(haystack_to_pages(&question).is_empty());
    }

    #[test]
    fn session_id_from_slug_strips_prefix() {
        assert_eq!(session_id_from_slug("chat/abc-1"), "abc-1");
        assert_eq!(session_id_from_slug("bare"), "bare");
        // Only the FIRST slash is the separator.
        assert_eq!(session_id_from_slug("chat/a/b"), "a/b");
    }

    #[test]
    fn slug_sanitizer_collapses_non_ascii_and_case() {
        assert_eq!(sanitize_session_id_for_slug("A_b.C"), "a-b-c");
        assert_eq!(sanitize_session_id_for_slug("x y"), "x-y");
    }
}
