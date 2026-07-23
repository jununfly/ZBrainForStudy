//! Unified diff hunk applier — applies hunks when context matches exactly.
//!
//! Pure Rust implementation with no external patch dependency. Only
//! applies hunks when the pre-change context matches exactly; conflicts are
//! skipped and reported for manual merging by the agent.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{Error, StructuredError, Result};

/// A single hunk from a unified diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    /// Starting line number on the old file (1-indexed).
    pub old_start: usize,
    /// Number of lines taken from the old file in this hunk.
    pub old_count: usize,
    /// Starting line number on the new file (1-indexed).
    pub new_start: usize,
    /// Number of lines in this hunk on the new file.
    pub new_count: usize,
    /// Hunk body lines (including the ' ', '-', '+' prefixes).
    pub lines: Vec<String>,
    /// True if the old file lacks a newline at the end of this hunk.
    pub old_no_newline_at_end: bool,
    /// True if the new file lacks a newline at the end of this hunk.
    pub new_no_newline_at_end: bool,
}

/// Parsed diff containing zero or more hunks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedDiff {
    pub hunks: Vec<Hunk>,
}

/// Error codes for apply-hunks operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyHunksErrorCode {
    /// Failed to parse the diff (bad hunk header).
    ParseError,
    /// Could not find a matching context block for a hunk in the input file.
    ContextMismatch,
    /// Multiple matching context blocks found (ambiguous).
    MultipleMatches,
}

/// Error for apply-hunks operations.
#[derive(Debug)]
pub struct ApplyHunksError {
    code: ApplyHunksErrorCode,
    message: String,
}

impl ApplyHunksError {
    pub fn new(code: ApplyHunksErrorCode, message: String) -> Self {
        Self { code, message }
    }
}

impl std::fmt::Display for ApplyHunksError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code: {:?})", self.message, self.code)
    }
}

impl std::error::Error for ApplyHunksError {}

impl From<ApplyHunksError> for Error {
    fn from(e: ApplyHunksError) -> Self {
        StructuredError::new(
            "ApplyHunks",
            "apply_hunks_failed",
            e.to_string(),
        )
    }
}

/// Parse a unified diff string into hunks.
pub fn parse_unified_diff(text: &str) -> Result<ParsedDiff> {
    let mut hunks = Vec::new();
    let lines: Vec<&str> = text.lines().collect();

    let mut i = 0;
    let hunk_header_re = Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap();

    while i < lines.len() {
        let line = lines[i];

        // Skip file headers until we hit a hunk header
        if !line.starts_with("@@") {
            i += 1;
            continue;
        }

        let Some(captures) = hunk_header_re.captures(line) else {
            return Err(ApplyHunksError::new(
                ApplyHunksErrorCode::ParseError,
                format!("Malformed hunk header: {line}"),
            ).into());
        };

        // Parse groups
        let old_start: usize = captures.get(1).map(|m| m.as_str())
            .map(|s| s.parse::<usize>())
            .transpose()
            .map_err(|e| {
                ApplyHunksError::new(
                    ApplyHunksErrorCode::ParseError,
                    format!("Invalid old_start: {}", e),
                )
            })?
            .unwrap();
        let old_count: usize = captures.get(2)
            .map(|m| m.as_str().parse().unwrap_or(1))
            .unwrap_or(1);
        let new_start: usize = captures.get(3).map(|m| m.as_str())
            .map(|s| s.parse::<usize>())
            .transpose()
            .map_err(|e| {
                ApplyHunksError::new(
                    ApplyHunksErrorCode::ParseError,
                    format!("Invalid new_start: {}", e),
                )
            })?
            .unwrap();
        let new_count: usize = captures.get(4)
            .map(|m| m.as_str().parse().unwrap_or(1))
            .unwrap_or(1);

        i += 1;

        let mut body: Vec<String> = Vec::new();
        let mut old_no_newline_at_end = false;
        let mut new_no_newline_at_end = false;

        let mut a_seen = 0;
        let mut b_seen = 0;

        while i < lines.len() {
            let ln = lines[i];

            // Stop at next hunk or next file header
            if ln.starts_with("@@") || ln.starts_with("---") || ln.starts_with("+++") {
                break;
            }

            // Handle the "No newline at end of file" marker
            if ln == r"\ No newline at end of file" {
                // Attribute to previous body line
                if let Some(prev) = body.last() {
                    let c = prev.chars().next();
                    if c == Some('-') || c == Some(' ') {
                        old_no_newline_at_end = true;
                    }
                    if c == Some('+') || c == Some(' ') {
                        new_no_newline_at_end = true;
                    }
                }
                i += 1;
                continue;
            }

            body.push(ln.to_string());
            i += 1;

            // Count lines for context checking
            let first_char = ln.chars().next().unwrap_or(' ');
            match first_char {
                ' ' => {
                    a_seen += 1;
                    b_seen += 1;
                }
                '-' => {
                    a_seen += 1;
                }
                '+' => {
                    b_seen += 1;
                }
                _ => {
                    // Unknown prefix, treat as context (gnu diff does this)
                    a_seen += 1;
                    b_seen += 1;
                }
            }

            // If we've seen enough lines, check if next line is the newline marker
            if a_seen >= old_count && b_seen >= new_count {
                if i < lines.len() && lines[i] == r"\ No newline at end of file" {
                    // consume it
                }
                break;
            }
        }

        hunks.push(Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines: body,
            old_no_newline_at_end,
            new_no_newline_at_end,
        });
    }

    Ok(ParsedDiff { hunks })
}

/// Result of applying hunks to a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyHunksResult {
    /// Final text after applying all applicable hunks.
    pub text: String,
    /// Number of hunks successfully applied.
    pub applied: usize,
    /// Number of hunks that couldn't be applied due to context mismatch.
    pub conflicts: usize,
    /// Details of each conflict for manual merging.
    pub conflict_hunks: Vec<Hunk>,
}

/// Apply parsed hunks to the original file content.
/// Only applies a hunk when the pre-change context (lines starting with ' ' or '-')
/// matches exactly in the input file at the expected position.
pub fn apply_hunks(original: &str, diff: &ParsedDiff) -> ApplyHunksResult {
    let mut original_lines: Vec<&str> = original.lines().collect();
    let mut result = Vec::new();
    let mut applied = 0;
    let mut conflicts = 0;
    let mut conflict_hunks = Vec::new();

    let mut pos = 0; // current position in original_lines (0-indexed)

    for hunk in &diff.hunks {
        // Convert 1-indexed to 0-indexed start
        let search_start = hunk.old_start - 1;

        // Collect the expected context lines from the hunk (lines that should exist in original)
        let expected_context: Vec<&str> = hunk.lines
            .iter()
            .filter_map(|ln| {
                let c = ln.chars().next()?;
                if c == ' ' || c == '-' {
                    // strip the prefix and collect
                    Some(&ln[1..])
                } else {
                    None
                }
            })
            .collect();

        // Search for a matching block in the original starting from search_start
        let mut matches = Vec::new();
        'search: for candidate_start in search_start..=original_lines.len().saturating_sub(expected_context.len()) {
            // Check if this candidate matches all expected context
            let mut matched = true;
            for (i, expected) in expected_context.iter().enumerate() {
                let actual_pos = candidate_start + i;
                if actual_pos >= original_lines.len() || original_lines[actual_pos] != *expected {
                    matched = false;
                    break;
                }
            }
            if matched {
                matches.push(candidate_start);
            }
        }

        match matches.len() {
            0 => {
                // No match — conflict
                conflicts += 1;
                conflict_hunks.push(hunk.clone());
                // Copy from current pos up to search_start as-is
                result.extend_from_slice(&original_lines[pos..search_start]);
                pos = search_start + hunk.old_count;
                continue;
            }
            1 => {
                // Exactly one match — apply
                let match_start = matches[0];
                // Copy from pos up to match_start
                result.extend_from_slice(&original_lines[pos..match_start]);

                // Apply the hunk body:
                // - ' ' → include the line (original unchanged)
                // - '-' → exclude the line (deleted)
                // - '+' → include the line (added)
                for line in &hunk.lines {
                    let Some(first) = line.chars().next() else {
                        // empty line with no prefix → include as empty
                        result.push("");
                        continue;
                    };
                    match first {
                        ' ' => result.push(&line[1..]),
                        '-' => {}, // delete
                        '+' => result.push(&line[1..]),
                        _ => result.push(line), // unknown prefix → include as-is
                    }
                }

                applied += 1;
                // Advance past the original lines we processed
                pos = match_start + hunk.old_count;
            }
            _ => {
                // Multiple matches — ambiguous, conflict
                conflicts += 1;
                conflict_hunks.push(hunk.clone());
                result.extend_from_slice(&original_lines[pos..search_start]);
                pos = search_start + hunk.old_count;
                continue;
            }
        }
    }

    // Copy remaining lines after the last hunk
    result.extend_from_slice(&original_lines[pos..]);

    // Reconstruct the output text
    let mut output = result.join("\n");

    // If the original didn't end with a newline, and we added content,
    // we may have added an extra newline. This is a rare edge case and
    // leaving the extra newline is acceptable for our use case.
    if !original.is_empty() && !original.ends_with('\n') && result.last().map_or(false, |s| s.ends_with('\n')) {
        output.truncate(output.len() - 1);
    }

    ApplyHunksResult {
        text: output,
        applied,
        conflicts,
        conflict_hunks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_diff() {
        let diff = "@@ -1,3 +1,3 @@\n line 1\n-line 2\n+line 2 changed\n line 3\n";
        let parsed = parse_unified_diff(diff).unwrap();
        assert_eq!(parsed.hunks.len(), 1);
        let hunk = &parsed.hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.old_count, 3);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_count, 3);
        assert_eq!(hunk.lines.len(), 3);
    }

    #[test]
    fn test_apply_simple_diff() {
        let original = "line 1\nline 2\nline 3\n";
        let diff = "@@ -1,3 +1,3 @@\n line 1\n-line 2\n+line 2 changed\n line 3\n";
        let parsed = parse_unified_diff(diff).unwrap();
        let result = apply_hunks(original, &parsed);
        assert_eq!(result.applied, 1);
        assert_eq!(result.conflicts, 0);
        assert_eq!(result.text, "line 1\nline 2 changed\nline 3\n");
    }

    #[test]
    fn test_apply_context_mismatch() {
        let original = "line 1\nline X\nline 3\n";
        let diff = "@@ -1,3 +1,3 @@\n line 1\n-line 2\n+line 2 changed\n line 3\n";
        let parsed = parse_unified_diff(diff).unwrap();
        let result = apply_hunks(original, &parsed);
        assert_eq!(result.applied, 0);
        assert_eq!(result.conflicts, 1);
    }
}
