//! Token budget enforcement for search results (roadmap 1-3-1).
//!
//! Ported from the TypeScript `src/core/search/token-budget.ts`. Uses a
//! deliberately cheap `char / 4` heuristic instead of a real tokenizer
//! (accurate within ~10-15% for English, ~5-25% for mixed code/Unicode).
//! Overshoot is intentional — this is a safety budget, not a precise
//! measurement.
//!
//! Pure module. Zero deps, zero allocations beyond the result vector.

/// Cheap char/4 token estimate. Returns 0 for empty strings.
///
/// Mirrors TS `estimateTokens` which counts JavaScript `string.length`
/// (UTF-16 code units, effectively character count for BMP). Rust
/// [`str::len`] counts bytes, not chars, so we use
/// [`str::chars().count()`] to match the TS behaviour. Rounds UP (`ceil`)
/// so a 1-char string still costs at least 1 token.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    // ceil(chars/4) via (chars + 3) / 4 — avoids floating point.
    (text.chars().count() as u64 + 3) / 4
}

/// Metadata returned by [`enforce_token_budget`], matching the TS
/// `TokenBudgetMeta` interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudgetMeta {
    /// Token budget that was applied (sanitized from caller input).
    pub budget: u64,
    /// Cumulative token cost of the returned items.
    pub used: u64,
    /// Count of items dropped to fit the budget.
    pub dropped: usize,
    /// Count of items actually kept.
    pub kept: usize,
}

/// Greedy top-down budget enforcement. Walks `items` in order, accumulates token
/// costs via `cost_fn`, and stops as soon as adding the next item would exceed
/// `budget`. Items are NOT re-ranked — caller's order is preserved.
///
/// Edge cases (mirror TS):
/// - `budget == 0` or `items` empty: returns all items unchanged, `dropped = 0`.
/// - First item alone exceeds budget: returns `[]`, `dropped = N`, `kept = 0`.
#[must_use]
pub fn enforce_token_budget<T>(
    items: &[T],
    budget: u64,
    cost_fn: impl Fn(&T) -> u64,
) -> (Vec<T>, TokenBudgetMeta)
where
    T: Clone,
{
    if budget == 0 || items.is_empty() {
        let total: u64 = items.iter().map(|i| cost_fn(i)).sum();
        return (
            items.to_vec(),
            TokenBudgetMeta {
                budget: 0,
                used: total,
                dropped: 0,
                kept: items.len(),
            },
        );
    }

    let mut kept: Vec<T> = Vec::with_capacity(items.len());
    let mut used: u64 = 0;
    for item in items {
        let cost = cost_fn(item);
        if used + cost > budget {
            break;
        }
        kept.push(item.clone());
        used += cost;
    }

    let kept_len = kept.len();
    (
        kept,
        TokenBudgetMeta {
            budget,
            used,
            dropped: items.len() - kept_len,
            kept: kept_len,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- estimate_tokens ----

    #[test]
    fn estimate_empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_rounds_up_for_short_strings() {
        assert_eq!(estimate_tokens("a"), 1); // ceil(1/4) = 1
        assert_eq!(estimate_tokens("ab"), 1); // ceil(2/4) = 1
        assert_eq!(estimate_tokens("abc"), 1); // ceil(3/4) = 1
        assert_eq!(estimate_tokens("abcd"), 1); // ceil(4/4) = 1
        assert_eq!(estimate_tokens("abcde"), 2); // ceil(5/4) = 2
    }

    #[test]
    fn estimate_typical_english_prose() {
        // "hello world" = 11 chars → ceil(11/4) = 3
        assert_eq!(estimate_tokens("hello world"), 3);
    }

    #[test]
    fn estimate_cjk_is_correct() {
        // "你好" = 2 chars → ceil(2/4) = 1 (matches TS behaviour).
        // char/4 deliberately underestimates CJK; safety budget should
        // be set larger by the caller.
        assert_eq!(estimate_tokens("你好"), 1);
        // 4 CJK chars = 1 token
        assert_eq!(estimate_tokens("你好世界"), 1);
    }

    // ---- enforce_token_budget ----

    fn item_cost(_: &u64) -> u64 {
        50
    }
    fn item_cost_id(i: &u64) -> u64 {
        *i
    }

    #[test]
    fn enforce_empty_returns_empty() {
        let items: Vec<u64> = vec![];
        let (results, meta) = enforce_token_budget(&items, 100, item_cost);
        assert!(results.is_empty());
        assert_eq!(meta.budget, 0);
        assert_eq!(meta.used, 0);
        assert_eq!(meta.dropped, 0);
        assert_eq!(meta.kept, 0);
    }

    #[test]
    fn enforce_zero_budget_returns_all_unchanged() {
        let items = vec![10, 20, 30];
        let (results, meta) = enforce_token_budget(&items, 0, item_cost_id);
        assert_eq!(results, items);
        assert_eq!(meta.budget, 0);
        assert_eq!(meta.used, 60); // 10+20+30
        assert_eq!(meta.dropped, 0);
        assert_eq!(meta.kept, 3);
    }

    #[test]
    fn enforce_fits_all_under_budget() {
        let items = vec![10, 20, 30];
        let (results, meta) = enforce_token_budget(&items, 100, item_cost_id);
        assert_eq!(results, items);
        assert_eq!(meta.budget, 100);
        assert_eq!(meta.used, 60);
        assert_eq!(meta.dropped, 0);
        assert_eq!(meta.kept, 3);
    }

    #[test]
    fn enforce_stops_at_budget_boundary() {
        // 50 + 50 + 50 = 150. Budget=120 → keeps first two, drops third.
        let items = vec![50, 50, 50];
        let (results, meta) = enforce_token_budget(&items, 120, item_cost_id);
        assert_eq!(results, vec![50, 50]);
        assert_eq!(meta.budget, 120);
        assert_eq!(meta.used, 100);
        assert_eq!(meta.dropped, 1);
        assert_eq!(meta.kept, 2);
    }

    #[test]
    fn enforce_exact_budget_fit_keeps_all() {
        let items = vec![40, 60];
        let (results, meta) = enforce_token_budget(&items, 100, item_cost_id);
        assert_eq!(results, items);
        assert_eq!(meta.used, 100);
        assert_eq!(meta.dropped, 0);
    }

    #[test]
    fn enforce_first_item_exceeds_budget_drops_all() {
        let items = vec![200, 10];
        let (results, meta) = enforce_token_budget(&items, 100, item_cost_id);
        assert!(results.is_empty());
        assert_eq!(meta.budget, 100);
        assert_eq!(meta.used, 0);
        assert_eq!(meta.dropped, 2);
        assert_eq!(meta.kept, 0);
    }

    #[test]
    fn enforce_second_item_exceeds_keeps_first() {
        // 30 fits, next 80 pushes to 110 → break, keep only first.
        let items = vec![30, 80];
        let (results, meta) = enforce_token_budget(&items, 100, item_cost_id);
        assert_eq!(results, vec![30]);
        assert_eq!(meta.used, 30);
        assert_eq!(meta.dropped, 1);
        assert_eq!(meta.kept, 1);
    }

    #[test]
    fn enforce_preserves_order() {
        let items = vec![10, 80, 5, 50, 90];
        // 10+80=90, +5=95, +50=145 > 120 → keep [10,80,5]
        let (results, meta) = enforce_token_budget(&items, 120, item_cost_id);
        assert_eq!(results, vec![10, 80, 5]);
        assert_eq!(meta.used, 95);
        assert_eq!(meta.dropped, 2);
        assert_eq!(meta.kept, 3);
    }
}
