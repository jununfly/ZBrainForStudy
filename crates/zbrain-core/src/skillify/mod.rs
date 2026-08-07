//! skillify — mechanical scaffold generator for `zbrain skillify scaffold`.
//!
//! Ported from `src/core/skillify/{generator,templates}.ts`. Pure file-tree
//! generator: [`plan_scaffold`] computes a [`ScaffoldPlan`] (dry-run, no
//! writes) and [`apply_scaffold`] materializes it. Idempotency contract
//! (D-CX-7): `--force` regenerates stub files but NEVER re-appends a resolver
//! row that already references this skill path — the resolver append is
//! content-idempotent.
//!
//! The `check` half of the `skillify` namespace (the 11-item audit) is *not*
//! here — it is tracked separately by roadmap node `1-1-1` and remains in
//! `src/commands/skillify-check.ts` for now.

pub mod generator;
pub mod templates;

pub use generator::{
    apply_scaffold, build_resolver_append, detect_existing_resolver_row, plan_scaffold,
    ScaffoldFile, ScaffoldFileKind, ScaffoldOptions, ScaffoldPlan, SkillifyScaffoldError,
    SKILL_NAME_PATTERN,
};
pub use templates::{
    resolver_row, routing_eval_template, script_template, skill_md_template, test_template,
    ScaffoldVars, SKILLIFY_STUB_MARKER,
};
