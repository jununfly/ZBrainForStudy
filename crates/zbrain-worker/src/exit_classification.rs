//! Worker exit classification (roadmap 1-2-5).
//!
//! Pure function with no side effects. Mirrors TS `exit-classification.ts`.
//! Used by `ChildWorkerSupervisor` to decide backoff strategy after a child
//! process exits.

/// Outcome of classifying a child process exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// Code 0 — worker exited cleanly (normal shutdown, no crash).
    Clean,
    /// Code non-zero or killed by signal — counted as a crash for backoff.
    Crash,
}

/// Classify a worker exit by its code and optional signal.
/// Mirrors TS `classifyWorkerExit`.
#[must_use]
pub fn classify_exit(code: Option<i32>, signal: Option<i32>) -> ExitClass {
    if signal.is_some() {
        return ExitClass::Crash;
    }
    match code {
        Some(0) | None => ExitClass::Clean,
        _ => ExitClass::Crash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_zero_is_clean() {
        assert_eq!(classify_exit(Some(0), None), ExitClass::Clean);
    }

    #[test]
    fn code_nonzero_is_crash() {
        assert_eq!(classify_exit(Some(1), None), ExitClass::Crash);
    }

    #[test]
    fn signal_always_crash() {
        // SIGTERM (15) with code 0 — still a crash
        assert_eq!(classify_exit(Some(0), Some(15)), ExitClass::Crash);
        // SIGKILL (9) with null code
        assert_eq!(classify_exit(None, Some(9)), ExitClass::Crash);
    }

    #[test]
    fn null_code_no_signal_is_clean() {
        assert_eq!(classify_exit(None, None), ExitClass::Clean);
    }
}
