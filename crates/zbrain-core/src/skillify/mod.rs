//! skillify — `zbrain skillify` namespace.
//!
//! Two halves, both ported from the original TS:
//!   - [`generator`] / [`templates`] — mechanical scaffold generator
//!     (`zbrain skillify scaffold`). Pure file-tree generator:
//!     [`plan_scaffold`] computes a [`ScaffoldPlan`] (dry-run, no writes) and
//!     [`apply_scaffold`] materializes it. Idempotency contract (D-CX-7):
//!     `--force` regenerates stub files but NEVER re-appends a resolver row
//!     that already references this skill path — the resolver append is
//!     content-idempotent.
//!   - [`check`] — post-task audit (`zbrain skillify check`), the 12-item
//!     checklist previously in `src/commands/skillify-check.ts`.
//!   - [`receipt`] — minimal cross-modal-eval receipt lookup used by item 11.

pub mod check;
pub mod generator;
pub mod receipt;
pub mod templates;

pub use check::{
    derive_root, resolve_skills_dir, run_skillify_check_target, CheckItem, CheckResult,
};
pub use generator::{
    apply_scaffold, build_resolver_append, detect_existing_resolver_row, plan_scaffold,
    ScaffoldFile, ScaffoldFileKind, ScaffoldOptions, ScaffoldPlan, SkillifyScaffoldError,
    SKILL_NAME_PATTERN,
};
pub use receipt::{describe_receipt_status, find_receipt_for_skill, sha8, ReceiptStatus};
pub use templates::{
    resolver_row, routing_eval_template, script_template, skill_md_template, test_template,
    ScaffoldVars, SKILLIFY_STUB_MARKER,
};
