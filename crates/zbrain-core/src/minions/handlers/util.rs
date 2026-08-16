//! Shared helpers for minion handlers that are intentionally *not* wired:
//! dead/orphaned job types left over from the TS command deletion (option C)
//! that have no Rust verb or are pglite dead tech. They fail loudly with a
//! pointer to `docs/plans/MIGRATION.md` rather than silently returning
//! `not_implemented`.

use crate::Result;

/// Build the "this job type is unsupported / wontfix" error for an orphaned
/// handler. `gap_id` is the `MIGRATION.md § 4` row (e.g. `"G80"`) documenting why.
#[must_use]
pub(crate) fn unsupported_job(name: &str, gap_id: &str) -> crate::Error {
    crate::Error::new(
        "Unsupported",
        name,
        &format!(
            "minion job '{name}' is not available: the underlying TS command was removed \
             (option C) and has no Rust replacement. See docs/plans/MIGRATION.md ({gap_id})."
        ),
    )
}
