//! LongMemEval intent classifier.
//!
//! Sibling of [`crate::think::intent`] with one key addition: it prefers the
//! dataset's `question_type` field (LongMemEval ships these labels populated)
//! before falling back to the shared regex set. For datasets without
//! `question_type`, the fallback is byte-identical to think's classifier
//! because both call the same function — no drift.
//!
//! Port of TS `src/eval/longmemeval/intent.ts` (v0.40.2.0).

use crate::think::intent::{classify_intent as classify_by_text, ThinkIntent};

use super::adapter::LongMemEvalQuestion;

/// Map LongMemEval's `question_type` field to the 3-bucket think intent.
///
/// Dataset labels (as of May 2026):
///
/// | label                         | intent             |
/// |-------------------------------|--------------------|
/// | `temporal-reasoning`          | `Temporal`         |
/// | `knowledge-update`            | `KnowledgeUpdate`  |
/// | `single-session-user`         | `Other`            |
/// | `single-session-assistant`    | `Other`            |
/// | `multi-session`               | `Other`            |
/// | `single-session-preference`   | `Other`            |
/// | anything else                 | `None` (fall through to the regex classifier) |
fn map_dataset_question_type(question_type: &str) -> Option<ThinkIntent> {
    match question_type.trim().to_lowercase().as_str() {
        "temporal-reasoning" => Some(ThinkIntent::Temporal),
        "knowledge-update" => Some(ThinkIntent::KnowledgeUpdate),
        "single-session-user"
        | "single-session-assistant"
        | "multi-session"
        | "single-session-preference" => Some(ThinkIntent::Other),
        _ => None,
    }
}

/// Classify a LongMemEval question.
///
/// Prefers the dataset's `question_type` label when it is one we recognize;
/// falls back to the shared regex classifier otherwise. Returns
/// [`ThinkIntent::Other`] for any question that shouldn't trigger trajectory
/// routing.
#[must_use]
pub fn classify_intent(question: &LongMemEvalQuestion) -> ThinkIntent {
    if let Some(from_type) = map_dataset_question_type(&question.question_type) {
        return from_type;
    }
    classify_by_text(&question.question)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(question_type: &str, question: &str) -> LongMemEvalQuestion {
        serde_json::from_value(serde_json::json!({
            "question_id": "q",
            "question_type": question_type,
            "question": question,
        }))
        .expect("fixture must deserialize")
    }

    #[test]
    fn dataset_label_wins_over_text() {
        // The text alone would classify as KnowledgeUpdate ("changed"), but the
        // dataset label pins it to Temporal.
        let out = classify_intent(&q("temporal-reasoning", "what changed?"));
        assert_eq!(out, ThinkIntent::Temporal);
    }

    #[test]
    fn knowledge_update_label_maps() {
        assert_eq!(
            classify_intent(&q("knowledge-update", "anything")),
            ThinkIntent::KnowledgeUpdate
        );
    }

    #[test]
    fn other_labels_map_to_other_even_when_text_is_temporal() {
        for label in [
            "single-session-user",
            "single-session-assistant",
            "multi-session",
            "single-session-preference",
        ] {
            assert_eq!(
                classify_intent(&q(label, "when did I last visit?")),
                ThinkIntent::Other,
                "label {label} must pin to Other"
            );
        }
    }

    #[test]
    fn label_match_is_case_and_whitespace_insensitive() {
        assert_eq!(
            classify_intent(&q("  Temporal-Reasoning ", "x")),
            ThinkIntent::Temporal
        );
    }

    #[test]
    fn unknown_label_falls_back_to_regex_classifier() {
        assert_eq!(
            classify_intent(&q("brand-new-bucket", "when did I last visit?")),
            ThinkIntent::Temporal
        );
        assert_eq!(
            classify_intent(&q("brand-new-bucket", "she moved to Berlin")),
            ThinkIntent::KnowledgeUpdate
        );
        assert_eq!(
            classify_intent(&q("brand-new-bucket", "describe the plan")),
            ThinkIntent::Other
        );
    }

    #[test]
    fn empty_label_falls_back_to_regex_classifier() {
        assert_eq!(
            classify_intent(&q("", "when did we meet?")),
            ThinkIntent::Temporal
        );
    }
}
