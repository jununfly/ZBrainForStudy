/**
 * skillpack/harvest_lint.rs — privacy linter for `zbrain skillpack harvest`.
 *
 * Reads `~/.zbrain/harvest-private-patterns.txt` (one regex per line,
 * user-maintained) plus a small built-in default list of patterns that
 * commonly leak when harvesting from a personal fork into zbrain core:
 *
 *   - `\bWintermute\b` — the canonical private fork name (CLAUDE.md
 *     explicitly bans this from zbrain core)
 *   - common email regex
 *   - common Slack channel pattern (`#channel-name`)
 *
 * Matches → throws `PrivacyLintError` with `hits[]` listing each
 * `file:line: matched-pattern` entry. The harvest runner rolls back
 * the copy on this signal.
 *
 * Malformed regex in the patterns file → fail loud at load time so
 * the user fixes their config before any harvest.
 */

use std::fs;
use std::path::{Path, PathBuf};
use regex::{Regex, RegexBuilder};

#[derive(Debug, thiserror::Error)]
pub enum PrivacyLintError {
    #[error("Privacy lint found {0:?} match(es) in harvested content. Harvest rolled back.")]
    LintErrors(Vec<String>),

    #[error("Invalid regex pattern in config: {pattern} — {detail}")]
    InvalidPattern { pattern: String, detail: String },

    #[error("IO error reading patterns: {0}")]
    Io(#[from] std::io::Error),
}

/// Default patterns shipped with zbrain (CLAUDE.md responsible-disclosure rule).
const DEFAULT_PRIVATE_PATTERNS: &[&str] = &[
    r"\bWintermute\b",
    // Email regex (RFC-5322-lite — good enough for harvest-time scrubbing).
    r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b",
    // Slack channel: whitespace/line-start, then `#alnum-with-dashes` (len ≥ 3).
    r"(?:^|\s)#[a-z0-9][a-z0-9_\-]{2,}\b",
];

/// A compiled pattern with its source string.
#[derive(Debug, Clone)]
pub struct CompiledPattern {
    pub regex: Regex,
    pub source: String,
}

/// Load patterns: user file (if present) + defaults. Each pattern
/// compiled to a regex; malformed regex throws at load time.
pub fn load_patterns(patterns_path: Option<&Path>) -> Result<Vec<CompiledPattern>, PrivacyLintError> {
    let mut lines: Vec<String> = Vec::new();

    if let Some(path) = patterns_path {
        if path.exists() {
            let raw = fs::read_to_string(path)?;
            for line in raw.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.starts_with('#') {
                    continue; // line comment
                }
                lines.push(trimmed.to_string());
            }
        }
    }

    // Append defaults AFTER user patterns so user-defined ones can be
    // tried first (e.g. for performance on patterns the user knows will
    // hit). Order otherwise doesn't matter — we report all hits.
    lines.extend(DEFAULT_PRIVATE_PATTERNS.iter().map(|&s| s.to_string()));

    let mut patterns = Vec::new();
    for line in lines {
        match RegexBuilder::new(&line).build() {
            Ok(regex) => patterns.push(CompiledPattern { regex, source: line }),
            Err(e) => {
                let path_str = patterns_path
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "<defaults>".to_string());
                return Err(PrivacyLintError::InvalidPattern {
                    pattern: line,
                    detail: format!("{}: {}", path_str, e),
                });
            }
        }
    }

    Ok(patterns)
}

/// Run the privacy linter against a list of harvested file paths.
/// Throws `PrivacyLintError` (with `hits[]`) on any match. No-op when
/// patterns + files yield zero hits.
pub fn run_privacy_lint(file_paths: &[PathBuf], patterns_path: Option<&Path>) -> Result<(), PrivacyLintError> {
    let patterns = load_patterns(patterns_path)?;
    if patterns.is_empty() {
        return Ok(());
    }

    let mut hits = Vec::new();

    for file in file_paths {
        if !file.exists() {
            continue;
        }
        let content = match fs::read_to_string(file) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for (line_idx, line) in content.lines().enumerate() {
            for pat in &patterns {
                if pat.regex.is_match(line) {
                    let file_str = file.to_string_lossy();
                    hits.push(format!("{}:{}: matched /{}/", file_str, line_idx + 1, pat.source));
                }
            }
        }
    }

    if !hits.is_empty() {
        let msg = format!(
            "Privacy lint found {} match(es) in harvested content. Harvest rolled back. \
             Edit your skill, run the editorial genericization, or add a pattern exception \
             to {}.",
            hits.len(),
            patterns_path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "~/.zbrain/harvest-private-patterns.txt".to_string())
        );
        Err(PrivacyLintError::LintErrors(hits))
    } else {
        Ok(())
    }
}
