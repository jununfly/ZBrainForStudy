//! Retry backoff calculation. Faithful port of TS `src/core/minions/backoff.ts`.
//!
//! - **Exponential**: `2^(attempts_made - 1) * backoff_delay`, floored at
//!   `attempts_made - 1 >= 0`.
//! - **Fixed**: constant `backoff_delay`.
//!
//! Both then apply symmetric jitter of `±(delay * backoff_jitter)` and clamp to
//! `>= 0`. From Sidekiq's formula with a BullMQ-style jitter parameter.
//!
//! ## Jitter injection
//!
//! The TS version calls `Math.random()` directly, which makes the jittered
//! result untestable. We factor the random draw behind a `unit ∈ [0, 1)`
//! parameter: [`calculate_backoff`] is the production entry point (draws from
//! the thread RNG, matching TS), and [`calculate_backoff_with_unit`] is the
//! pure, deterministic core the tests pin. This is a deep-module split — one
//! tiny seam turns an RNG-coupled function into a spec-testable one.

use zbrain_core::minions::types::BackoffType;

/// Inputs to a backoff computation. The subset of a `MinionJob` the formula
/// reads — mirrors the TS `Pick<MinionJob, 'backoff_type' | 'backoff_delay' |
/// 'backoff_jitter' | 'attempts_made'>`.
#[derive(Debug, Clone, Copy)]
pub struct BackoffInput {
    pub backoff_type: BackoffType,
    pub backoff_delay: i64,
    pub backoff_jitter: f64,
    /// Attempt number this backoff is *for* (the worker passes
    /// `attempts_made + 1` — the attempt about to be retried), matching the TS
    /// call site `worker.ts` L828.
    pub attempts_made: i32,
}

/// Production entry point: draws jitter from the thread RNG, faithful to the TS
/// `Math.random()`. Returns the delay in milliseconds.
#[must_use]
pub fn calculate_backoff(input: &BackoffInput) -> f64 {
    let unit = rand::random::<f64>(); // [0, 1), same domain as Math.random()
    calculate_backoff_with_unit(input, unit)
}

/// Deterministic core. `unit` stands in for one `Math.random()` draw in
/// `[0, 1)`. Separated out so tests can pin the jitter and assert the exact
/// curve. Returns the delay in milliseconds, clamped to `>= 0`.
#[must_use]
pub fn calculate_backoff_with_unit(input: &BackoffInput, unit: f64) -> f64 {
    let base = input.backoff_delay as f64;
    let mut delay = match input.backoff_type {
        BackoffType::Exponential => {
            let exp = (input.attempts_made - 1).max(0);
            2f64.powi(exp) * base
        }
        BackoffType::Fixed => base,
    };

    if input.backoff_jitter > 0.0 {
        let jitter_range = delay * input.backoff_jitter;
        // TS: delay += random()*range*2 - range  -> symmetric ±range.
        delay += unit * jitter_range * 2.0 - jitter_range;
    }

    delay.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(t: BackoffType, delay: i64, jitter: f64, attempts: i32) -> BackoffInput {
        BackoffInput {
            backoff_type: t,
            backoff_delay: delay,
            backoff_jitter: jitter,
            attempts_made: attempts,
        }
    }

    // behavior 1 --------------------------------------------------------------
    #[test]
    fn fixed_backoff_is_constant_without_jitter() {
        let got = calculate_backoff_with_unit(&input(BackoffType::Fixed, 1000, 0.0, 5), 0.5);
        assert_eq!(got, 1000.0);
    }

    // behavior 2 --------------------------------------------------------------
    #[test]
    fn exponential_backoff_doubles_per_attempt() {
        // 2^(n-1) * delay, jitter off.
        let d = |n| calculate_backoff_with_unit(&input(BackoffType::Exponential, 1000, 0.0, n), 0.5);
        assert_eq!(d(1), 1000.0); // 2^0 * 1000
        assert_eq!(d(2), 2000.0); // 2^1 * 1000
        assert_eq!(d(3), 4000.0); // 2^2 * 1000
        assert_eq!(d(4), 8000.0); // 2^3 * 1000
        // attempts_made <= 1 floors the exponent at 0 (never negative).
        assert_eq!(d(0), 1000.0);
    }

    // behavior 3 --------------------------------------------------------------
    #[test]
    fn jitter_stays_within_symmetric_range_and_nonnegative() {
        let inp = input(BackoffType::Fixed, 1000, 0.2, 1);
        // unit=0.0 -> delay - range (lower bound); unit just-below-1 -> upper.
        let low = calculate_backoff_with_unit(&inp, 0.0);
        let mid = calculate_backoff_with_unit(&inp, 0.5);
        let high = calculate_backoff_with_unit(&inp, 0.999_999);
        assert_eq!(low, 800.0); // 1000 - (1000*0.2)
        assert_eq!(mid, 1000.0); // 1000 + 0
        assert!((high - 1200.0).abs() < 0.01); // ~1000 + 200
        assert!(low >= 0.0 && mid >= 0.0 && high >= 0.0);
    }

    #[test]
    fn jitter_clamps_to_zero_when_range_would_go_negative() {
        // Huge jitter fraction, unit=0 -> delay - (delay*jitter) is negative,
        // clamped to 0.
        let got = calculate_backoff_with_unit(&input(BackoffType::Fixed, 100, 5.0, 1), 0.0);
        assert_eq!(got, 0.0);
    }
}
