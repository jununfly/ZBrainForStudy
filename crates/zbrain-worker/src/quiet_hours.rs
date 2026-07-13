//! Quiet-hours gate for Minions — evaluated at claim time, not dispatch.
//!
//! Mirrors TS `evaluateQuietHours`.  Pure function: the caller supplies the
//! current hour (0–23) in the job's configured timezone; the gate returns
//! whether the job should be allowed, skipped, or deferred.  Keeping the
//! timezone→hour conversion out of this module avoids pulling in chrono-tz.
//!
//! Windows may wrap midnight: `{start: 22, end: 6}` means 22:00–06:00 next
//! morning.  Straight-line and wrap-around windows are handled identically.

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct QuietHoursConfig {
    /// 0–23; window starts at this local hour inclusive.
    pub start: u32,
    /// 0–23; window ends at this local hour exclusive.
    pub end: u32,
    /// IANA timezone, e.g. "America/Los_Angeles".
    pub tz: String,
    /// What to do when the job fires inside the window.
    pub policy: QuietHoursPolicy,
}

impl Default for QuietHoursPolicy {
    fn default() -> Self {
        QuietHoursPolicy::Defer
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietHoursPolicy {
    /// Drop the job (don't retry).
    Skip,
    /// Re-queue for later.
    Defer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietHoursVerdict {
    /// Job can run — not inside a quiet window.
    Allow,
    /// Drop the job (skip policy).
    Skip,
    /// Re-queue for later (defer policy).
    Defer,
}

// ---------------------------------------------------------------------------
// Main API
// ---------------------------------------------------------------------------

/// Evaluate a quiet-hours config against the current hour (0–23) in the
/// job's configured timezone.  Returns `Allow` when `current_hour` is outside
/// the window, or `Skip`/`Defer` according to policy when inside.
///
/// `current_hour` is the hour in the job's configured IANA timezone.  The
/// caller is responsible for the tz→hour conversion (avoids chrono-tz dep).
pub fn evaluate_quiet_hours(cfg: Option<&QuietHoursConfig>, current_hour: u32) -> QuietHoursVerdict {
    let cfg = match cfg {
        Some(c) => c,
        None => return QuietHoursVerdict::Allow,
    };

    // Fail-open: invalid config → allow.
    if !is_valid(cfg) {
        return QuietHoursVerdict::Allow;
    }

    let in_window = if cfg.start <= cfg.end {
        // Straight-line window e.g. 9–17.
        current_hour >= cfg.start && current_hour < cfg.end
    } else {
        // Midnight-wrap window e.g. 22–6.
        current_hour >= cfg.start || current_hour < cfg.end
    };

    if !in_window {
        return QuietHoursVerdict::Allow;
    }

    match cfg.policy {
        QuietHoursPolicy::Skip => QuietHoursVerdict::Skip,
        QuietHoursPolicy::Defer => QuietHoursVerdict::Defer,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_valid(cfg: &QuietHoursConfig) -> bool {
    if cfg.start > 23 || cfg.end > 23 {
        return false;
    }
    if cfg.start == cfg.end {
        return false; // zero-width window is ambiguous
    }
    if cfg.tz.is_empty() {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(start: u32, end: u32, tz: &str, policy: QuietHoursPolicy) -> QuietHoursConfig {
        QuietHoursConfig {
            start,
            end,
            tz: tz.to_string(),
            policy,
        }
    }

    // --- behavior 5: null config → Allow -----------------------------------

    #[test]
    fn null_config_is_allow() {
        assert_eq!(evaluate_quiet_hours(None, 3), QuietHoursVerdict::Allow);
    }

    // --- behavior 6a: midnight-wrap window → Defer -------------------------

    #[test]
    fn midnight_wrap_defer_inside() {
        let c = cfg(22, 6, "UTC", QuietHoursPolicy::Defer);
        // 3:00 UTC is inside 22:00–6:00.
        assert_eq!(evaluate_quiet_hours(Some(&c), 3), QuietHoursVerdict::Defer);
        assert_eq!(evaluate_quiet_hours(Some(&c), 23), QuietHoursVerdict::Defer);
        assert_eq!(evaluate_quiet_hours(Some(&c), 0), QuietHoursVerdict::Defer);
    }

    #[test]
    fn midnight_wrap_defer_outside() {
        let c = cfg(22, 6, "UTC", QuietHoursPolicy::Defer);
        // 14:00 UTC is outside 22:00–6:00.
        assert_eq!(evaluate_quiet_hours(Some(&c), 14), QuietHoursVerdict::Allow);
        assert_eq!(evaluate_quiet_hours(Some(&c), 21), QuietHoursVerdict::Allow);
        assert_eq!(evaluate_quiet_hours(Some(&c), 7), QuietHoursVerdict::Allow);
    }

    // --- behavior 7: normal window → Skip ----------------------------------

    #[test]
    fn normal_window_skip_inside() {
        let c = cfg(9, 17, "UTC", QuietHoursPolicy::Skip);
        assert_eq!(evaluate_quiet_hours(Some(&c), 9), QuietHoursVerdict::Skip);
        assert_eq!(evaluate_quiet_hours(Some(&c), 16), QuietHoursVerdict::Skip);
    }

    #[test]
    fn normal_window_skip_outside() {
        let c = cfg(9, 17, "UTC", QuietHoursPolicy::Skip);
        assert_eq!(evaluate_quiet_hours(Some(&c), 8), QuietHoursVerdict::Allow);
        assert_eq!(evaluate_quiet_hours(Some(&c), 17), QuietHoursVerdict::Allow);
    }

    // --- behavior 8: default policy is Defer (no explicit policy field) ----

    #[test]
    fn default_policy_is_defer() {
        let c = QuietHoursConfig {
            start: 0,
            end: 6,
            tz: "UTC".to_string(),
            policy: Default::default(), // serde default → Defer
        };
        assert_eq!(c.policy, QuietHoursPolicy::Defer);
    }

    // --- behavior 9: invalid config → Allow (fail-open) --------------------

    #[test]
    fn zero_width_window_is_allow() {
        let c = cfg(5, 5, "UTC", QuietHoursPolicy::Defer);
        assert_eq!(evaluate_quiet_hours(Some(&c), 5), QuietHoursVerdict::Allow);
    }

    #[test]
    fn out_of_range_hour_is_allow() {
        let c = cfg(9, 25, "UTC", QuietHoursPolicy::Defer); // end > 23
        assert_eq!(evaluate_quiet_hours(Some(&c), 10), QuietHoursVerdict::Allow);
    }

    #[test]
    fn empty_tz_is_allow() {
        let c = cfg(9, 17, "", QuietHoursPolicy::Defer);
        assert_eq!(evaluate_quiet_hours(Some(&c), 10), QuietHoursVerdict::Allow);
    }
}
