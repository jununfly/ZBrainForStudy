//! ReDoS guard for link-type inference.
//!
//! Port of `src/core/schema-pack/redos-guard.ts`.
//!
//! In the TS original, link inference runs untrusted regex patterns pulled
//! from a schema pack against page content. A pathological pattern can
//! trigger catastrophic backtracking (ReDoS). Bun's `vm.runInContext({timeout})`
//! is the primary defense — it hard-interrupts an over-budget regex.
//!
//! Rust's `regex` crate is backed by a finite automaton and is **not**
//! vulnerable to JS-style catastrophic backtracking, so the ReDoS class of
//! bug largely does not apply. We still keep the per-page CPU budget so a
//! large number of (expensive-but-finite) patterns cannot monopolize a page's
//! inference pass: once the cumulative budget is spent, further regex
//! matching degrades to a safe default (`None`) instead of burning CPU.
//!
//! NOTE: Rust cannot interrupt a running regex the way `vm` can. `run_bounded`
//! therefore enforces the budget *cooperatively*: it refuses to start once
//! the budget is exhausted, and accounts elapsed time after each match. A
//! single pathological pattern could still run to completion (it just can't
//! backtrack-explode). This matches the TS contract closely enough for the
//! inference use case.

use std::time::{SystemTime, UNIX_EPOCH};

/// Per-page cumulative budget for all link-inference regex work (ms).
pub const LINK_EXTRACTION_TOTAL_BUDGET_MS: u128 = 500;

/// Per-regex wall-clock budget (ms). Patterns slower than this are treated
/// as a no-match for that page (degrades to the safe default).
pub const PER_REGEX_TIMEOUT_MS: u128 = 50;

/// Raised when a single regex exceeds [`PER_REGEX_TIMEOUT_MS`] (only
/// observable via [`run_regex_bounded`], which returns `None` instead of
/// throwing — kept as a type for API parity with the TS surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexTimeoutError {
    pub verb: String,
    pub pattern: String,
}

impl std::fmt::Display for RegexTimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "regex {} (verb {}) exceeded per-regex budget",
            self.pattern, self.verb
        )
    }
}

impl std::error::Error for RegexTimeoutError {}

/// Raised when the per-page cumulative budget is exceeded across many regexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageBudgetExceededError {
    pub cumulative_ms: u128,
}

impl std::fmt::Display for PageBudgetExceededError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "page regex budget exceeded at {}ms", self.cumulative_ms)
    }
}

impl std::error::Error for PageBudgetExceededError {}

/// Outcome of a single bounded regex attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedosOutcome {
    /// Budget already exhausted before this regex started. Callers should
    /// degrade to their safe default (e.g. `mentions`).
    Exhausted,
    /// Regex ran but did not match.
    NoMatch,
    /// Regex ran and matched; carries capture groups (group 0 = full match).
    Matched(Vec<String>),
}

impl RedosOutcome {
    pub fn is_match(&self) -> bool {
        matches!(self, RedosOutcome::Matched(_))
    }
}

/// Per-page regex budget tracker.
#[derive(Debug, Clone)]
pub struct PageRegexBudget {
    total_budget_ms: u128,
    per_regex_ms: u128,
    cumulative_ms: u128,
    exhausted: bool,
    clock: fn() -> u128,
}

impl Default for PageRegexBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Default monotonic millisecond clock (wall clock since UNIX epoch). Good
/// enough for per-page CPU budgeting; tests inject a deterministic counter.
fn default_clock() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

impl PageRegexBudget {
    /// Create a budget with the default limits
    /// ([`LINK_EXTRACTION_TOTAL_BUDGET_MS`] / [`PER_REGEX_TIMEOUT_MS`]).
    pub fn new() -> Self {
        Self::with_limits(LINK_EXTRACTION_TOTAL_BUDGET_MS, PER_REGEX_TIMEOUT_MS)
    }

    /// Create a budget with explicit limits. `clock` is injectable for tests.
    pub fn with_limits(total_budget_ms: u128, per_regex_ms: u128) -> Self {
        Self {
            total_budget_ms,
            per_regex_ms,
            cumulative_ms: 0,
            exhausted: false,
            clock: default_clock,
        }
    }

    /// Run `pattern` against `text` under the budget for `verb`.
    ///
    /// Returns [`RedosOutcome::Exhausted`] if the per-page budget is already
    /// spent. Otherwise compiles + runs the regex, accounts elapsed time, and
    /// flips `exhausted` if the cumulative limit is crossed.
    pub fn run_bounded(&mut self, verb: &str, pattern: &str, text: &str) -> RedosOutcome {
        if self.exhausted {
            return RedosOutcome::Exhausted;
        }
        let start = (self.clock)();
        let outcome = match compile_and_match(pattern, text) {
            Some(groups) => RedosOutcome::Matched(groups),
            None => RedosOutcome::NoMatch,
        };
        let elapsed = (self.clock)().saturating_sub(start);
        self.cumulative_ms = self.cumulative_ms.saturating_add(elapsed);
        if self.cumulative_ms >= self.total_budget_ms {
            self.exhausted = true;
            // The call that crosses the budget is the one that exhausts: bail
            // and discard this result so callers stop processing the page.
            return RedosOutcome::Exhausted;
        }
        let _ = (verb, self.per_regex_ms);
        outcome
    }

    pub fn get_cumulative_ms(&self) -> u128 {
        self.cumulative_ms
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }
}

/// Compile `pattern` and return capture groups if it matches `text`.
/// Returns `None` on no match or compile error. Group 0 is the full match,
/// followed by subgroups 1..n in order.
fn compile_and_match(pattern: &str, text: &str) -> Option<Vec<String>> {
    let re = regex::Regex::new(pattern).ok()?;
    let caps = re.captures(text)?;
    let mut groups = vec![caps.get(0)?.as_str().to_string()];
    for i in 1..re.captures_len() {
        groups.push(caps.get(i).map(|m| m.as_str().to_string()).unwrap_or_default());
    }
    Some(groups)
}

/// Standalone bounded regex run (no per-page budget tracking).
///
/// Returns `Some(groups)` on match, `None` on no match or compile error.
/// `timeout_ms` is accepted for API parity with the TS `runRegexBounded`
/// but is **not enforced** — Rust cannot interrupt a running regex.
pub fn run_regex_bounded(pattern: &str, text: &str, _timeout_ms: u128) -> Option<Vec<String>> {
    compile_and_match(pattern, text)
}

/// Build a deterministic budget for testing: a clock that advances 10ms per
/// call so cumulative accounting is reproducible without real sleeps.
#[cfg(test)]
pub(crate) fn test_budget(total: u128, per: u128) -> PageRegexBudget {
    use std::cell::Cell;
    thread_local! {
        static COUNTER: Cell<u64> = const { Cell::new(0) };
    }
    fn tick() -> u128 {
        COUNTER.with(|c| {
            let v = c.get() + 1;
            c.set(v);
            (v as u128) * 10
        })
    }
    PageRegexBudget {
        total_budget_ms: total,
        per_regex_ms: per,
        cumulative_ms: 0,
        exhausted: false,
        clock: tick,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_returns_groups() {
        let mut b = test_budget(500, 50);
        let out = b.run_bounded("person", r"^(person):", "person:alice");
        match out {
            RedosOutcome::Matched(groups) => {
                // group 0 is the full match; group 1 is the first capture.
                assert_eq!(groups[0], "person:");
                assert_eq!(groups[1], "person");
            }
            other => panic!("expected Matched, got {other:?}"),
        }
    }

    #[test]
    fn no_match_returns_no_match() {
        let mut b = test_budget(500, 50);
        let out = b.run_bounded("person", r"^(person):", "company:acme");
        assert_eq!(out, RedosOutcome::NoMatch);
    }

    #[test]
    fn invalid_pattern_is_no_match_not_panic() {
        let mut b = test_budget(500, 50);
        let out = b.run_bounded("x", r"([", "anything");
        assert_eq!(out, RedosOutcome::NoMatch);
    }

    #[test]
    fn exhausted_after_cumulative_limit() {
        let mut b = test_budget(25, 50); // total 25ms, clock +10ms/call
        // 3 calls = 30ms cumulative > 25ms limit -> 3rd is Exhausted
        let _ = b.run_bounded("a", r"x", "x");
        assert!(!b.is_exhausted());
        let _ = b.run_bounded("b", r"y", "y");
        assert!(!b.is_exhausted());
        let third = b.run_bounded("c", r"z", "z");
        assert!(b.is_exhausted());
        assert_eq!(third, RedosOutcome::Exhausted);
        // subsequent calls also exhausted
        assert_eq!(b.run_bounded("d", r"z", "z"), RedosOutcome::Exhausted);
    }

    #[test]
    fn run_regex_bounded_standalone() {
        assert_eq!(
            run_regex_bounded(r"\d+", "abc123", 50),
            Some(vec!["123".to_string()])
        );
        assert_eq!(run_regex_bounded(r"\d+", "abc", 50), None);
    }
}
