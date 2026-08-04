pub mod extract_facts;
pub mod extract_atoms;
pub mod extract_takes;
pub mod propose_takes;
pub mod grade_takes;
pub mod conversation_facts_backfill;
pub mod emotional_weight;
pub mod recompute_emotional_weight;
pub mod synthesize_concepts;
pub mod schema_suggest;
pub mod patterns;
pub mod transcript_discovery;
pub mod context_budget;
pub mod synthesize;
pub mod auto_think;
pub mod drift;
pub mod consolidate;
// 1-6-6 phantom-redirect pre-pass modules (declared late: files existed but
// were never wired into the module tree, causing E0432 in downstream phases).
pub mod phantom_redirect;
pub mod phantom_audit;
pub mod resolve;
