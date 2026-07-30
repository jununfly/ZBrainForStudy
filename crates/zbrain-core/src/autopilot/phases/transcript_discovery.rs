//! Transcript discovery for the dream-cycle synthesize phase.
//!
//! Port of TS `src/core/cycle/transcript-discovery.ts` (295 lines). Pure
//! filesystem + crypto; no engine/DB dependency. Hermetic-testable with temp
//! directories.
//!
//! Walks a corpus directory for `.txt`/`.md` files, applies date-range filters,
//! size filters (`min_chars`), and word-boundary regex exclude patterns, then
//! returns file paths + content + sha256 `content_hash` so the caller can key
//! the verdict cache and dispatch one subagent per transcript.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use regex::Regex;
use sha2::{Digest, Sha256};

lazy_static::lazy_static! {
    static ref DATE_RE: Regex = Regex::new(r"^(\d{4}-\d{2}-\d{2})").unwrap();
    static ref WORD_BOUNDARY_HEURISTIC: Regex = Regex::new(r"^[a-zA-Z][a-zA-Z0-9_-]*$").unwrap();
    // DREAM marker: frontmatter `dream_generated: true` within first 2000 chars.
    static ref DREAM_OUTPUT_MARKER_RE: Regex = Regex::new(
        r"(?:\u{feff})?---\r?\n[\s\S]{0,2000}?dream_generated\s*:\s*true\b",
    )
    .unwrap();
    // LSD marker: `mode: lsd` (noise-by-design skip, D4).
    static ref LSD_OUTPUT_MARKER_RE: Regex = Regex::new(
        r#"(?:\u{feff})?---\r?\n[\s\S]{0,2000}?mode\s*:\s*(?:"|'|)lsd(?:"|'|)\s*(?:\r?\n|$)"#,
    )
    .unwrap();
    // Brainstorm marker: `mode: brainstorm` (saved by `zbrain brainstorm --save`).
    static ref BRAINSTORM_OUTPUT_MARKER_RE: Regex = Regex::new(
        r#"(?:\u{feff})?---\r?\n[\s\S]{0,2000}?mode\s*:\s*(?:"|'|)brainstorm(?:"|'|)\s*(?:\r?\n|$)"#,
    )
    .unwrap();
}

const DEFAULT_MIN_CHARS: usize = 2000;

/// Directories skipped at descent time (mirrors TS `PRUNE_DIR_NAMES`).
const PRUNE_DIR_NAMES: &[&str] = &["node_modules", ".raw", "ops"];

/// A transcript discovered on disk, ready for the verdict cache + fan-out.
#[derive(Debug, Clone)]
pub struct DiscoveredTranscript {
    /// Absolute path to the transcript file.
    pub file_path: PathBuf,
    /// sha256(content), full hex; callers slice as needed (e.g. first 16).
    pub content_hash: String,
    /// Raw transcript text.
    pub content: String,
    /// Filename basename without extension; used as a topic-slug seed.
    pub basename: String,
    /// Inferred date if the basename matches `YYYY-MM-DD...` (or None).
    pub inferred_date: Option<String>,
}

/// Options for [`discover_transcripts`].
#[derive(Debug, Clone, Default)]
pub struct DiscoverOpts {
    /// Source directory. Required.
    pub corpus_dir: PathBuf,
    /// Optional second source.
    pub meeting_transcripts_dir: Option<PathBuf>,
    /// Skip transcripts shorter than this many characters. Default 2000.
    pub min_chars: Option<usize>,
    /// Word-boundary regex strings; bare words auto-wrapped in `\b...\b`.
    pub exclude_patterns: Vec<String>,
    /// Restrict to a single date (YYYY-MM-DD basename match).
    pub date: Option<String>,
    /// Inclusive range start (YYYY-MM-DD).
    pub from: Option<String>,
    /// Inclusive range end (YYYY-MM-DD).
    pub to: Option<String>,
    /// Disable the self-consumption guard (never auto-applied).
    pub bypass_guard: bool,
}

/// True iff content carries the LSD frontmatter marker (D4 noise-by-design skip).
pub fn is_lsd_output(content: &str) -> bool {
    LSD_OUTPUT_MARKER_RE.is_match(content)
}

/// True iff content carries the brainstorm frontmatter marker.
pub fn is_brainstorm_output(content: &str) -> bool {
    BRAINSTORM_OUTPUT_MARKER_RE.is_match(content)
}

/// Self-consumption guard: identity-marker check against dream output, extended
/// to also skip `mode: lsd` per D4. `bypass` is the explicit escape hatch for
/// the dream-output check only — LSD output is ALWAYS skipped.
pub fn is_dream_output(content: &str, bypass: bool) -> bool {
    if is_lsd_output(content) {
        return true;
    }
    if bypass {
        return false;
    }
    DREAM_OUTPUT_MARKER_RE.is_match(content)
}

/// Auto-wrap bare-word patterns in `\b<word>\b`. Full regex honored verbatim.
/// Bad regex from user config is skipped with a stderr warning (never crashes).
pub fn compile_exclude_patterns(patterns: &[String]) -> Vec<Regex> {
    let mut out = Vec::new();
    for p in patterns {
        if p.is_empty() {
            continue;
        }
        let src = if WORD_BOUNDARY_HEURISTIC.is_match(p) {
            format!(r"\b{}\b", p)
        } else {
            p.clone()
        };
        match Regex::new(&src) {
            Ok(re) => out.push(re),
            Err(e) => {
                eprintln!("[dream] invalid exclude_pattern '{}': {}", p, e);
            }
        }
    }
    out
}

fn hash_content(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn is_in_date_range(date: Option<&str>, opts: &DiscoverOpts) -> bool {
    if opts.date.is_none() && opts.from.is_none() && opts.to.is_none() {
        return true;
    }
    let date = match date {
        Some(d) => d,
        None => return false,
    };
    if let Some(want) = &opts.date {
        if date != want.as_str() {
            return false;
        }
    }
    if let Some(from) = &opts.from {
        if date < from.as_str() {
            return false;
        }
    }
    if let Some(to) = &opts.to {
        if date > to.as_str() {
            return false;
        }
    }
    true
}

fn matches_any_exclude(text: &str, patterns: &[Regex]) -> bool {
    patterns.iter().any(|re| re.is_match(text))
}

/// Should a directory entry be pruned at descent time? Mirrors TS `pruneDir`.
fn should_prune(name: &str, dir_path: &Path) -> bool {
    if name.is_empty() {
        return false; // TS: empty name stays descendable
    }
    if name.starts_with('.') {
        return true; // hidden dirs (.git/.obsidian/.cache/.raw...)
    }
    if PRUNE_DIR_NAMES.contains(&name) {
        return true;
    }
    if name.ends_with(".raw") {
        return true;
    }
    // git submodule: .git present as a FILE inside the candidate dir
    if dir_path.join(".git").is_file() {
        return true;
    }
    false
}

/// Recursive walk accepting both `.txt` and `.md`; prunes vendor/hidden dirs at
/// descent time. Returns paths sorted for deterministic re-runs.
fn list_text_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true; // never prune the root corpus dir itself (TS pruneDir only filters subdirs)
            }
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !should_prune(&name, e.path())
            } else {
                true
            }
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().is_file() {
            let name = entry.file_name().to_string_lossy();
            if name.ends_with(".txt") || name.ends_with(".md") {
                out.push(entry.path().to_path_buf());
            }
        }
    }
    out.sort();
    out
}

fn basename_of(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let ext = if name.ends_with(".md") { ".md" } else { ".txt" };
    name.strip_suffix(ext).unwrap_or(&name).to_string()
}

fn infer_date(basename: &str) -> Option<String> {
    DATE_RE
        .captures(basename)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Discover transcripts from the configured corpus dirs, applying filters.
///
/// Skips files that aren't `.txt`/`.md`, are outside the date window, below
/// `min_chars`, carry the `dream_generated` marker (unless `bypass_guard`), or
/// match any exclude pattern. Returns sorted by `file_path` for deterministic
/// re-runs.
pub fn discover_transcripts(opts: &DiscoverOpts) -> Vec<DiscoveredTranscript> {
    let min_chars = opts.min_chars.unwrap_or(DEFAULT_MIN_CHARS);
    let bypass = opts.bypass_guard;
    let exclude_res = compile_exclude_patterns(&opts.exclude_patterns);

    let mut dirs: Vec<&Path> = Vec::new();
    if opts.corpus_dir.exists() {
        dirs.push(&opts.corpus_dir);
    }
    if let Some(d) = &opts.meeting_transcripts_dir {
        if d.exists() {
            dirs.push(d);
        }
    }

    let mut results: Vec<DiscoveredTranscript> = Vec::new();
    for dir in &dirs {
        for file_path in list_text_files(dir) {
            let base_name = basename_of(&file_path);
            let inferred_date = infer_date(&base_name);
            if !is_in_date_range(inferred_date.as_deref(), opts) {
                continue;
            }
            let content = match std::fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if content.chars().count() < min_chars {
                continue;
            }
            if is_dream_output(&content, bypass) {
                eprintln!(
                    "[dream] skipped {}: dream_generated marker (self-consumption guard)",
                    base_name
                );
                continue;
            }
            if matches_any_exclude(&content, &exclude_res) {
                continue;
            }
            results.push(DiscoveredTranscript {
                file_path: file_path.clone(),
                content_hash: hash_content(&content),
                content,
                basename: base_name,
                inferred_date,
            });
        }
    }

    results.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    results
}

/// Read a single ad-hoc transcript file (`zbrain dream --input <file>`). Bypasses
/// the corpus-dir scan and date filters but still applies `min_chars` + exclude
/// patterns + self-consumption guard unless `bypass_guard` is set.
pub fn read_single_transcript(
    file_path: &Path,
    min_chars: Option<usize>,
    exclude_patterns: &[String],
    bypass_guard: bool,
) -> Option<DiscoveredTranscript> {
    let min_chars = min_chars.unwrap_or(DEFAULT_MIN_CHARS);
    let exclude_res = compile_exclude_patterns(exclude_patterns);
    let content = std::fs::read_to_string(file_path).ok()?;
    if content.chars().count() < min_chars {
        return None;
    }
    if is_dream_output(&content, bypass_guard) {
        let bn = basename_of(file_path);
        eprintln!(
            "[dream] readSingleTranscript skipped {}: dream_generated marker (self-consumption guard)",
            bn
        );
        return None;
    }
    if matches_any_exclude(&content, &exclude_res) {
        return None;
    }
    let base_name = basename_of(file_path);
    let inferred_date = infer_date(&base_name);
    Some(DiscoveredTranscript {
        file_path: file_path.to_path_buf(),
        content_hash: hash_content(&content),
        content,
        basename: base_name,
        inferred_date,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn discovers_txt_and_md_sorted_by_path() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(dir.path(), "2026-07-30-a.txt", &"x".repeat(2500));
        write_file(dir.path(), "2026-07-29-b.md", &"y".repeat(2500));
        // a .log file should be ignored
        write_file(dir.path(), "ignore.log", &"z".repeat(2500));
        let opts = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let got = discover_transcripts(&opts);
        let names: Vec<String> = got.iter().map(|t| t.basename.clone()).collect();
        assert_eq!(
            names,
            vec!["2026-07-29-b".to_string(), "2026-07-30-a".to_string()]
        );
        // content_hash is full sha256 hex (64 chars)
        assert_eq!(got[0].content_hash.len(), 64);
        assert_eq!(got[0].inferred_date.as_deref(), Some("2026-07-29"));
    }

    #[test]
    fn min_chars_filters_short_files() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(dir.path(), "2026-07-30-short.txt", &"x".repeat(50));
        write_file(dir.path(), "2026-07-30-long.txt", &"x".repeat(3000));
        let opts = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let names: Vec<String> = discover_transcripts(&opts)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(names, vec!["2026-07-30-long".to_string()]);
    }

    #[test]
    fn date_filter_works() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(dir.path(), "2026-07-28-a.txt", &"x".repeat(2500));
        write_file(dir.path(), "2026-07-30-b.txt", &"x".repeat(2500));
        write_file(dir.path(), "2026-07-31-c.txt", &"x".repeat(2500));
        // single date
        let opts = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            date: Some("2026-07-30".to_string()),
            ..Default::default()
        };
        let names: Vec<String> = discover_transcripts(&opts)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(names, vec!["2026-07-30-b".to_string()]);
        // range
        let opts2 = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            from: Some("2026-07-29".to_string()),
            to: Some("2026-07-30".to_string()),
            ..Default::default()
        };
        let names2: Vec<String> = discover_transcripts(&opts2)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(names2, vec!["2026-07-30-b".to_string()]);
        // with a date filter active, a file with no inferable date is dropped
        write_file(dir.path(), "no-date.txt", &"x".repeat(2500));
        let opts3 = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            date: Some("2026-07-30".to_string()),
            ..Default::default()
        };
        let names3: Vec<String> = discover_transcripts(&opts3)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(names3, vec!["2026-07-30-b".to_string()]);
    }

    #[test]
    fn dream_output_marker_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let dream = format!("---\ntitle: x\ndream_generated: true\n---\n\n{}", "y".repeat(2500));
        write_file(dir.path(), "2026-07-30-dream.txt", &dream);
        let normal = format!("Some transcript text\n\n{}", "z".repeat(2500));
        write_file(dir.path(), "2026-07-30-normal.txt", &normal);
        let opts = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let names: Vec<String> = discover_transcripts(&opts)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(names, vec!["2026-07-30-normal".to_string()]);
    }

    #[test]
    fn lsd_output_always_skipped_even_with_bypass() {
        let dir = tempfile::TempDir::new().unwrap();
        let lsd = format!("---\nmode: lsd\n---\n\n{}", "y".repeat(2500));
        write_file(dir.path(), "2026-07-30-lsd.txt", &lsd);
        let opts = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            bypass_guard: true,
            ..Default::default()
        };
        assert!(discover_transcripts(&opts).is_empty());
    }

    #[test]
    fn exclude_patterns_skip_word_boundary() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(
            dir.path(),
            "2026-07-30-therapy.txt",
            &format!("therapy session notes here\n\n{}", "x".repeat(2500)),
        );
        write_file(
            dir.path(),
            "2026-07-30-work.txt",
            &format!("work standup meeting notes\n\n{}", "x".repeat(2500)),
        );
        // bare word → wrapped in \b...\b, case-insensitive
        let opts = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            exclude_patterns: vec!["therapy".to_string()],
            ..Default::default()
        };
        let names: Vec<String> = discover_transcripts(&opts)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(names, vec!["2026-07-30-work".to_string()]);
    }

    #[test]
    fn prunes_vendor_and_hidden_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        // node_modules inner file should be skipped
        fs::create_dir_all(dir.path().join("node_modules/sub")).unwrap();
        write_file(
            &dir.path().join("node_modules/sub"),
            "2026-07-30-nm.txt",
            &"x".repeat(2500),
        );
        // .git inner file should be skipped
        fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        write_file(
            &dir.path().join(".git/objects"),
            "2026-07-30-git.txt",
            &"x".repeat(2500),
        );
        // a legit nested dir is walked
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        write_file(
            &dir.path().join("notes"),
            "2026-07-30-note.txt",
            &"x".repeat(2500),
        );
        let opts = DiscoverOpts {
            corpus_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let names: Vec<String> = discover_transcripts(&opts)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(names, vec!["2026-07-30-note".to_string()]);
    }

    #[test]
    fn meeting_dir_merged_with_corpus() {
        // Use ONE TempDir with two subdirs to avoid WSL 9P fs races between
        // two independent TempDir roots (single-dir tests are deterministic).
        let dir = tempfile::TempDir::new().unwrap();
        let corpus = dir.path().join("corpus");
        let meeting = dir.path().join("meeting");
        fs::create_dir_all(&corpus).unwrap();
        fs::create_dir_all(&meeting).unwrap();
        write_file(&corpus, "2026-07-30-c.txt", &"x".repeat(2500));
        write_file(&meeting, "2026-07-30-m.txt", &"x".repeat(2500));
        let opts = DiscoverOpts {
            corpus_dir: corpus,
            meeting_transcripts_dir: Some(meeting),
            ..Default::default()
        };
        let names: Vec<String> = discover_transcripts(&opts)
            .into_iter()
            .map(|t| t.basename)
            .collect();
        assert_eq!(
            names,
            vec!["2026-07-30-c".to_string(), "2026-07-30-m".to_string()]
        );
    }

    #[test]
    fn read_single_transcript_adhoc() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = write_file(dir.path(), "adhoc.txt", &format!("hello\n\n{}", "x".repeat(2500)));
        let t = read_single_transcript(&p, None, &[], false).unwrap();
        assert_eq!(t.basename, "adhoc");
        assert_eq!(t.content_hash.len(), 64);
        // too short → None
        let short = write_file(dir.path(), "short.txt", "tiny");
        assert!(read_single_transcript(&short, None, &[], false).is_none());
    }

    #[test]
    fn content_hash_is_deterministic() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = write_file(dir.path(), "2026-07-30-h.txt", "same content here");
        let a = read_single_transcript(&p, Some(0), &[], false).unwrap();
        let b = read_single_transcript(&p, Some(0), &[], false).unwrap();
        assert_eq!(a.content_hash, b.content_hash);
        assert_eq!(a.content_hash, hash_content("same content here"));
    }
}
