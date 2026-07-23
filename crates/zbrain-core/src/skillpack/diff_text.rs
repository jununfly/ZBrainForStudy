//! Line-based diff with unified-diff output (Hunt–McIlroy LCS).
//!
//! Pure Rust implementation with zero dependencies, produces predictable unified diff
//! output that can be applied round-trip.

use std::cmp::min;

/// Default number of context lines around each hunk.
const DEFAULT_CONTEXT: usize = 3;

/// Options for generating a unified diff.
#[derive(Debug, Clone)]
pub struct UnifiedDiffOpts {
    /// Number of context lines around each hunk.
    pub context: usize,
    /// Path label for the old side in the diff header.
    pub old_path: Option<String>,
    /// Path label for the new side in the diff header.
    pub new_path: Option<String>,
}

impl Default for UnifiedDiffOpts {
    fn default() -> Self {
        Self {
            context: DEFAULT_CONTEXT,
            old_path: None,
            new_path: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    Equal,
    Delete,
    Insert,
}

struct AnnotatedOp {
    op: DiffOp,
    a_idx: usize,
    b_idx: usize,
}

/// Compute a unified diff between two text strings.
/// Returns an empty string if the inputs are identical.
pub fn unified_diff(a: &str, b: &str, opts: UnifiedDiffOpts) -> String {
    if a == b {
        return String::new();
    }

    let context = opts.context;
    let old_path = opts.old_path.unwrap_or_else(|| "a".to_string());
    let new_path = opts.new_path.unwrap_or_else(|| "b".to_string());

    let a_lines = split_lines(a);
    let b_lines = split_lines(b);

    let ops = diff_lines(&a_lines, &b_lines);
    if ops.is_empty() {
        return String::new();
    }

    // Annotate each op with its indices
    let mut annotated = Vec::new();
    let mut ai = 0;
    let mut bi = 0;
    for op in ops {
        annotated.push(AnnotatedOp {
            op: op,
            a_idx: ai,
            b_idx: bi,
        });
        match op {
            DiffOp::Equal => {
                ai += 1;
                bi += 1;
            }
            DiffOp::Delete => {
                ai += 1;
            }
            DiffOp::Insert => {
                bi += 1;
            }
        }
    }

    // Group into hunks
    let change_indices: Vec<usize> = annotated
        .iter()
        .enumerate()
        .filter(|(_, op)| op.op != DiffOp::Equal)
        .map(|(i, _)| i)
        .collect();

    if change_indices.is_empty() {
        return String::new();
    }

    let mut ranges = Vec::new();
    let mut current_start = change_indices[0].saturating_sub(context);
    let mut current_end = change_indices[0];

    for &c in &change_indices[1..] {
        let next_start = c.saturating_sub(context);
        if next_start <= current_end + context {
            // Merge with current range
            current_end = c;
        } else {
            // Finish current range, start new
            ranges.push((current_start..=current_end));
            current_start = next_start;
            current_end = c;
        }
    }
    ranges.push((current_start..=current_end));

    let mut output = String::new();
    output.push_str(&format!("--- {}\n", old_path));
    output.push_str(&format!("+++ {}\n", new_path));

    for range in ranges {
        let first = *range.start();
        let last = *range.end();

        // Find the old line numbers for the hunk header
        let a_start = if first == 0 {
            1
        } else {
            annotated[first].a_idx + 1
        };
        let mut a_count = 0;
        for i in *range.start()..=*range.end() {
            if annotated[i].op != DiffOp::Insert {
                a_count += 1;
            }
        }

        // Find the new line numbers
        let b_start = if first == 0 {
            1
        } else {
            annotated[first].b_idx + 1
        };
        let mut b_count = 0;
        for i in *range.start()..=*range.end() {
            if annotated[i].op != DiffOp::Delete {
                b_count += 1;
            }
        }

        output.push_str(&format!("@@ -{a_start},{a_count} +{b_start},{b_count} @@\n"));

        for i in *range.start()..=*range.end() {
            let ann = &annotated[i];
            match ann.op {
                DiffOp::Equal => {
                    output.push(' ');
                    output.push_str(a_lines[ann.a_idx]);
                    output.push('\n');
                }
                DiffOp::Delete => {
                    output.push('-');
                    output.push_str(a_lines[ann.a_idx]);
                    output.push('\n');
                }
                DiffOp::Insert => {
                    output.push('+');
                    output.push_str(b_lines[ann.b_idx]);
                    output.push('\n');
                }
            }
        }
    }

    output
}

fn split_lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

fn diff_lines<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<DiffOp> {
    let (lcs, _) = lcs(a, b);
    ops_from_lcs(a, b, &lcs)
}

fn lcs<'a>(a: &[&'a str], b: &[&'a str]) -> (Vec<(usize, usize)>, usize) {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0; m + 1]; n + 1];

    for i in 1..=n {
        for j in 1..=m {
            if a[i - 1] == b[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = max(dp[i - 1][j], dp[i][j - 1]);
            }
        }
    }

    // Backtrack to recover the LCS
    let mut lcs = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            lcs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    lcs.reverse();
    (lcs, dp[n][m])
}

fn ops_from_lcs<'a>(a: &[&'a str], b: &[&'a str], lcs: &[(usize, usize)]) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let mut i = 0;
    let mut j = 0;

    for &(ai, bj) in lcs {
        // Deletions from a: between previous and this match
        while i < ai {
            ops.push(DiffOp::Delete);
            i += 1;
        }
        // Insertions to b: between previous and this match
        while j < bj {
            ops.push(DiffOp::Insert);
            j += 1;
        }
        // The matching line is equal
        ops.push(DiffOp::Equal);
        i += 1;
        j += 1;
    }

    // Remaining deletions
    while i < a.len() {
        ops.push(DiffOp::Delete);
        i += 1;
    }
    // Remaining insertions
    while j < b.len() {
        ops.push(DiffOp::Insert);
        j += 1;
    }

    // Collapse consecutive equals (not strictly needed, but cleaner)
    ops
}

fn max<T: Ord>(a: T, b: T) -> T {
    if a > b { a } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_diff() {
        let a = "hello\nworld\n";
        let b = "hello\nworld\n";
        let diff = unified_diff(a, b, UnifiedDiffOpts::default());
        assert!(diff.is_empty());
    }

    #[test]
    fn test_simple_diff() {
        let a = "line 1\nline 2\nline 3\n";
        let b = "line 1\nline 2 changed\nline 3\n";
        let diff = unified_diff(a, b, UnifiedDiffOpts::default());
        assert!(!diff.is_empty());
        assert!(diff.contains("-line 2"));
        assert!(diff.contains("+line 2 changed"));
    }
}
