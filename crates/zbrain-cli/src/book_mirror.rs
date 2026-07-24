//! `zbrain book-mirror` — flagship of the v0.25.1 skills wave (Rust port).
//!
//! Takes pre-extracted chapter text + context, fans out N read-only subagents
//! (one per chapter), waits for all to complete, assembles the two-column
//! personalized analysis, and writes ONE `put_page` under
//! `media/books/<slug>-personalized` using the operator-trust path.
//!
//! ## Trust contract (D2/α + codex HIGH-1 fix)
//!
//! - Subagents are submitted with `allowed_tools: ["get_page", "search"]` only
//!   — they can READ the brain, but they CANNOT call `put_page`. This is
//!   enforced in [`SubagentHandler`](zbrain_core::minions::handlers::subagent)
//!   which intersects the requested tools with the brain allowlist (see G56).
//!   They produce markdown analysis text via their result; the CLI reads
//!   `job.result` and assembles the final page itself.
//! - The CLI calls `engine.put_page` once at the end with operator-level trust
//!   (no `via_subagent` flag), so the subagent namespace check doesn't apply.
//!   Untrusted EPUB content cannot prompt-inject any `people/*` page because
//!   subagents lack write access entirely.
//!
//! The skill (`skills/book-mirror/SKILL.md`) handles EPUB/PDF extraction and
//! invokes this CLI with `--chapters-dir` pointing at the extracted `.txt`.
//! Separation of concerns: skill prepares inputs, CLI is the trusted runtime.
//!
//! ## Executor
//!
//! Faithful to the TS command, this is self-contained: it runs an in-process
//! worker (see [`crate::inline_worker`]) to actually execute the fanned-out
//! subagent jobs, because the Rust CLI has no external executor.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use zbrain_core::ai::chat::instantiate_chat;
use zbrain_core::ai::resolver::resolve_recipe_strict;
use zbrain_core::engine::BrainEngine;
use zbrain_core::minions::queue::MinionQueue;
use zbrain_core::minions::types::{MinionJobInput, MinionJobStatus};
use zbrain_core::PageInput;

use crate::inline_worker::{run_subagent_jobs, InlineWorkerOpts};

const COST_PER_CHAPTER_OPUS: f64 = 0.30; // rough; depends on chapter length
const COST_PER_CHAPTER_SONNET: f64 = 0.06;
const DEFAULT_MAX_TURNS: i32 = 10;
const DEFAULT_WORKERS: u32 = 4; // queue concurrency hint

/// Arguments for `zbrain book-mirror`.
#[derive(Debug, clap::Parser)]
pub struct BookMirrorArgs {
    /// Directory containing chapter text files (.txt). Sorted alphabetically;
    /// chapter order = sort order.
    #[arg(long = "chapters-dir")]
    pub chapters_dir: String,

    /// Brain page slug (kebab-case, no leading slash). Output lands at
    /// media/books/<slug>-personalized.
    #[arg(long)]
    pub slug: String,

    /// Path to a context pack (USER.md + SOUL.md + memory excerpts). Embedded in
    /// every child subagent's prompt.
    #[arg(long = "context-file")]
    pub context_file: Option<String>,

    /// Book title (used in the assembled page header). Defaults to slug.
    #[arg(long)]
    pub title: Option<String>,

    /// Book author (used in frontmatter + page header).
    #[arg(long)]
    pub author: Option<String>,

    /// `provider:model` id for chapter analysis. Default anthropic:claude-opus-4-7.
    #[arg(long, default_value = "anthropic:claude-opus-4-7")]
    pub model: String,

    /// Per-chapter subagent turn budget.
    #[arg(long = "max-turns", default_value_t = DEFAULT_MAX_TURNS)]
    pub max_turns: i32,

    /// Per-chapter wall-clock timeout in ms.
    #[arg(long = "timeout-ms")]
    pub timeout_ms: Option<i64>,

    /// Skip the cost-estimate confirmation prompt.
    #[arg(long = "no-confirm", alias = "yes")]
    pub no_confirm: bool,

    /// Submit and wait; if the terminal is non-interactive this is implied.
    #[arg(long = "no-follow")]
    pub no_follow: bool,

    /// Validate inputs + print plan; submit nothing.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// One loaded chapter (pre-extracted `.txt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterEntry {
    pub index: usize,
    pub filename: String,
    pub text: String,
    pub word_count: usize,
}

/// Result of one chapter's analysis after the fan-out completes.
#[derive(Debug, Clone)]
pub struct ChapterAnalysis {
    pub index: usize,
    pub result: String,
    pub failed: bool,
    pub error: Option<String>,
}

// ── chapter loading (pure) ─────────────────────────────────

/// Load `.txt` chapter files from `dir`, sorted alphabetically. Chapter order is
/// the sort order. Errors if the dir is missing/not-a-dir/has no `.txt`.
pub fn load_chapters(dir: &Path) -> Result<Vec<ChapterEntry>, String> {
    if !dir.exists() {
        return Err(format!("--chapters-dir not found: {}", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("--chapters-dir is not a directory: {}", dir.display()));
    }
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read --chapters-dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|f| f.ends_with(".txt"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("No .txt files in --chapters-dir: {}", dir.display()));
    }
    let mut chapters = Vec::with_capacity(files.len());
    for (i, filename) in files.into_iter().enumerate() {
        let full_path = dir.join(&filename);
        let text = std::fs::read_to_string(&full_path)
            .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;
        let word_count = text.split_whitespace().count();
        chapters.push(ChapterEntry {
            index: i + 1,
            filename,
            text,
            word_count,
        });
    }
    Ok(chapters)
}

// ── cost estimate (pure) ───────────────────────────────────

/// Rough per-run cost in USD. Opus vs Sonnet is decided by the model string.
#[must_use]
pub fn estimate_cost(chapters: usize, model: &str) -> f64 {
    let per_chapter = if model.contains("opus") {
        COST_PER_CHAPTER_OPUS
    } else {
        COST_PER_CHAPTER_SONNET
    };
    chapters as f64 * per_chapter
}

// ── prompt assembly (pure) ─────────────────────────────────

/// Build the per-chapter subagent prompt. Mirrors the TS `buildChapterPrompt`.
#[must_use]
pub fn build_chapter_prompt(
    chapter: &ChapterEntry,
    total_chapters: usize,
    book_title: &str,
    book_author: Option<&str>,
    context_pack: Option<&str>,
) -> String {
    let author_line = book_author.map(|a| format!(" by {a}")).unwrap_or_default();
    let context_section = match context_pack {
        Some(pack) => format!("\n\n## READER CONTEXT\n\n{pack}\n\n"),
        None => "\n\n## READER CONTEXT\n\n(No context pack supplied; right column will be limited to brain-search-discoverable content.)\n\n".to_string(),
    };
    let idx = chapter.index;

    format!(
        r#"You are analyzing one chapter of "{book_title}"{author_line} for the user.

Your output is a markdown two-column table where the LEFT column preserves the chapter's actual content (stories, frameworks, statistics, named examples) and the RIGHT column maps each idea to the user's actual life using their words, situations, and patterns from the brain.

This is chapter {idx} of {total_chapters}.

## CHAPTER {idx} TEXT (full, do not summarize this away)

{text}
{context_section}

## OUTPUT

Return ONLY a single markdown section in this exact shape:

```
## Chapter {idx}: [Title from the chapter — extract or infer]

### Key Ideas
[2-4 sentence thesis of the chapter — what the author is actually arguing.]

| What the Author Says | How This Applies to You |
|---|---|
| [Detailed paragraph: a section/argument from the chapter, preserving stories, stats, frameworks, named examples. Use `<br><br>` for paragraph breaks within the cell.] | [Specific personal connection: name dates, people, exact quotes from the user, real situations. Same `<br><br>` for breaks.] |
| [Next section] | [Next mirror] |
| [4-10 rows depending on chapter density] |  |
```

## RULES

- LEFT column: preserve stories, stats, frameworks. Don't summarize away the texture.
- RIGHT column: use the user's actual words from READER CONTEXT. Name specific people, dates, situations. Read like a therapist who knows them.
- 4-10 rows per chapter. If a section honestly doesn't apply, write `*This section is less directly relevant because [specific reason].*` Don't force connections.
- Never generic ("This might apply if you've ever felt..."). Never sycophantic. Never preach.
- Use `<br><br>` for paragraph breaks inside table cells, not literal newlines.

You have {DEFAULT_MAX_TURNS} turns and read-only tools (get_page, search). You CANNOT call put_page — your output is the markdown text in your final message. The CLI assembles all chapters and writes the brain page.

When done, your final message should contain ONLY the `## Chapter {idx}: ...` section above. No preamble, no postscript, no commentary."#,
        text = chapter.text,
    )
}

/// Inputs for [`build_assembled_page`].
pub struct AssembleOpts<'a> {
    pub title: &'a str,
    pub author: Option<&'a str>,
    pub context_pack: Option<&'a str>,
    pub chapter_analyses: &'a [ChapterAnalysis],
    /// `YYYY-MM-DD` stamp for the frontmatter `date`.
    pub today: String,
}

/// Assemble the final page markdown (frontmatter + intro + chapter sections).
/// Mirrors the TS `buildAssembledPage`.
#[must_use]
pub fn build_assembled_page(opts: &AssembleOpts) -> String {
    let author_line = opts
        .author
        .map(|a| format!("\nauthor: \"{a}\""))
        .unwrap_or_default();

    let context_summary = match opts.context_pack {
        Some(pack) => {
            let joined = pack.lines().take(3).collect::<Vec<_>>().join(" ");
            joined.chars().take(200).collect::<String>()
        }
        None => "No reader-context pack supplied.".to_string(),
    };

    let frontmatter = format!(
        "---\ntitle: \"{title} — Personalized\"\ntype: book-analysis{author_line}\ndate: {date}\ncontext: \"{context}\"\ntags: [book, personalized, two-column]\n---",
        title = opts.title,
        date = opts.today,
        context = context_summary.replace('"', "\\\""),
    );

    let author_suffix = opts.author.map(|a| format!(" by {a}")).unwrap_or_default();
    let intro = format!(
        "# {title} — Personalized\n\n## What this is\n\nA chapter-by-chapter personalized analysis of *{title}*{author_suffix}. Each chapter is summarized in detail on the left and mirrored to the reader's actual life on the right, drawing on brain context.\n\nThis page was generated by `zbrain book-mirror`. Each chapter analysis came from a separate read-only subagent that had access to the chapter text and a reader-context pack but no write tools — so the brain wasn't modified during the per-chapter analysis. This page is the only artifact written.\n\n",
        title = opts.title,
    );

    let failed: Vec<&ChapterAnalysis> = opts.chapter_analyses.iter().filter(|a| a.failed).collect();
    let failed_header = if failed.is_empty() {
        String::new()
    } else {
        let body = failed
            .iter()
            .map(|a| {
                format!(
                    "> Chapter {}: analysis failed ({}). Re-run `zbrain book-mirror` to retry; idempotent on the same inputs.",
                    a.index,
                    a.error.as_deref().unwrap_or("unknown error")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        format!(
            "\n\n## Failed chapters ({})\n\n{}\n\n---\n",
            failed.len(),
            body
        )
    };

    let mut completed: Vec<&ChapterAnalysis> =
        opts.chapter_analyses.iter().filter(|a| !a.failed).collect();
    completed.sort_by_key(|a| a.index);
    let completed_body = completed
        .iter()
        .map(|a| a.result.trim())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    format!("{frontmatter}\n\n{intro}{failed_header}\n{completed_body}\n")
}

// ── orchestration (testable core) ──────────────────────────

/// The plan-level inputs threaded through [`orchestrate`].
pub struct BookMirrorPlan {
    pub slug: String,
    pub title: String,
    pub author: Option<String>,
    pub context_pack: Option<String>,
    pub model: String,
    pub max_turns: i32,
    pub timeout_ms: Option<i64>,
    /// `media/books/<slug>-personalized`.
    pub target_slug: String,
    pub concurrency: u32,
    /// `YYYY-MM-DD` used in the assembled frontmatter.
    pub today: String,
}

/// Outcome summary returned to the caller for reporting.
pub struct BookMirrorOutcome {
    pub target_slug: String,
    pub chapters_total: usize,
    pub chapters_completed: usize,
    pub chapters_failed: usize,
    pub bytes_written: usize,
}

/// Fan out one subagent job per chapter, execute them with an in-process worker,
/// assemble the two-column page, and write it via `engine.put_page` (operator
/// trust). Returns `Ok(None)` when every chapter failed (nothing written).
///
/// This is the testable core: pass an [`InMemoryEngine`](zbrain_core::InMemoryEngine)
/// and a stub `ChatProvider` to exercise the full submit→execute→assemble→write
/// path without a real LLM.
pub async fn orchestrate(
    engine: Arc<dyn BrainEngine>,
    provider: Arc<dyn zbrain_core::ai::chat::ChatProvider>,
    chapters: &[ChapterEntry],
    plan: &BookMirrorPlan,
) -> anyhow::Result<Option<BookMirrorOutcome>> {
    // Submit fan-out: N children, read-only tools (CODEX HIGH-1).
    let child_ids = {
        let queue = MinionQueue::new(&*engine);
        let mut ids = Vec::with_capacity(chapters.len());
        for ch in chapters {
            let data = json!({
                "prompt": build_chapter_prompt(
                    ch,
                    chapters.len(),
                    &plan.title,
                    plan.author.as_deref(),
                    plan.context_pack.as_deref(),
                ),
                "model": plan.model,
                "max_turns": plan.max_turns,
                // CODEX HIGH-1: read-only tool allowlist. Subagents cannot call
                // put_page or any mutating op; their only output is result text.
                "allowed_tools": ["get_page", "search"],
            });
            let input = MinionJobInput {
                name: "subagent".into(),
                data: Some(data),
                max_stalled: Some(3),
                // Loose idempotency: same chapter + slug → same key, so re-runs
                // dedup against the queue.
                idempotency_key: Some(format!("book-mirror:{}:ch-{}", plan.slug, ch.index)),
                timeout_ms: plan.timeout_ms,
                ..Default::default()
            };
            let job = queue.add(&input).await?;
            ids.push(job.id);
        }
        ids
    };

    // Execute all children in-process.
    let overall_deadline = Duration::from_millis(
        plan.timeout_ms
            .map(|t| (t as u64).saturating_mul(chapters.len() as u64).max(t as u64))
            .unwrap_or(30 * 60 * 1000),
    );
    let jobs = run_subagent_jobs(
        Arc::clone(&engine),
        provider,
        &child_ids,
        InlineWorkerOpts {
            concurrency: plan.concurrency.max(1),
            poll_ms: 200,
            overall_deadline,
        },
    )
    .await?;

    // Collect analyses in chapter order.
    let mut analyses = Vec::with_capacity(chapters.len());
    for (i, ch) in chapters.iter().enumerate() {
        let job = jobs.get(i).and_then(|j| j.as_ref());
        match job {
            Some(j) if j.status == MinionJobStatus::Completed => {
                let result = j
                    .result
                    .as_ref()
                    .and_then(|r| r.get("result"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                analyses.push(ChapterAnalysis {
                    index: ch.index,
                    result,
                    failed: false,
                    error: None,
                });
            }
            Some(j) => analyses.push(ChapterAnalysis {
                index: ch.index,
                result: String::new(),
                failed: true,
                error: Some(format!("job {} status={}", j.id, j.status.as_str())),
            }),
            None => analyses.push(ChapterAnalysis {
                index: ch.index,
                result: String::new(),
                failed: true,
                error: Some("job row disappeared".to_string()),
            }),
        }
    }

    let failed = analyses.iter().filter(|a| a.failed).count();
    let completed = analyses.len() - failed;
    if completed == 0 {
        return Ok(None);
    }

    let assembled = build_assembled_page(&AssembleOpts {
        title: &plan.title,
        author: plan.author.as_deref(),
        context_pack: plan.context_pack.as_deref(),
        chapter_analyses: &analyses,
        today: plan.today.clone(),
    });

    // Operator-trust put_page — no via_subagent flag; the CLI is the trusted
    // writer. Rust `put_page` stores compiled_truth verbatim (no frontmatter
    // parser), so the assembled markdown is the page body.
    let page_input = PageInput {
        page_type: "book-analysis".to_string(),
        title: format!("{} — Personalized", plan.title),
        compiled_truth: assembled.clone(),
        ..Default::default()
    };
    engine
        .put_page(&plan.target_slug, Some("default"), &page_input)
        .await?;

    Ok(Some(BookMirrorOutcome {
        target_slug: plan.target_slug.clone(),
        chapters_total: chapters.len(),
        chapters_completed: completed,
        chapters_failed: failed,
        bytes_written: assembled.len(),
    }))
}

// ── command entry ──────────────────────────────────────────

fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Prompt for confirmation on a TTY; refuse to spend from a non-TTY context.
fn confirm_interactive(estimate_usd: f64, chapters: usize) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "zbrain book-mirror: refusing to spend ~${:.2} on {} chapters from a non-TTY context. Pass --yes to confirm.",
            estimate_usd, chapters
        );
        return false;
    }
    eprint!(
        "\nThis will spawn {} subagent jobs at ~${:.2} each = ~${:.2} total.\nContinue? [y/N] ",
        chapters,
        estimate_usd / chapters as f64,
        estimate_usd
    );
    let _ = std::io::stderr().flush();
    let mut reply = String::new();
    if std::io::stdin().read_line(&mut reply).is_err() {
        return false;
    }
    let reply = reply.trim().to_lowercase();
    reply == "y" || reply == "yes"
}

/// Execute `zbrain book-mirror`.
pub async fn run_book_mirror(
    engine: Arc<dyn BrainEngine>,
    args: BookMirrorArgs,
) -> anyhow::Result<()> {
    // Validate.
    if !is_valid_slug(&args.slug) {
        anyhow::bail!(
            "invalid --slug \"{}\". Use kebab-case (a-z, 0-9, hyphens).",
            args.slug
        );
    }
    if let Some(ctx) = &args.context_file {
        if !Path::new(ctx).exists() {
            anyhow::bail!("--context-file not found: {ctx}");
        }
    }

    // Load chapters.
    let chapters = load_chapters(Path::new(&args.chapters_dir))
        .map_err(|e| anyhow::anyhow!("zbrain book-mirror: {e}"))?;

    let context_pack = match &args.context_file {
        Some(p) => Some(std::fs::read_to_string(p)?),
        None => None,
    };
    let book_title = args.title.clone().unwrap_or_else(|| args.slug.clone());
    let target_slug = format!("media/books/{}-personalized", args.slug);

    eprint!(
        "\nzbrain book-mirror — plan\n  slug:        {}\n  output:      {}\n  chapters:    {} (from {})\n  context:     {}\n  model:       {}\n  max_turns:   {}\n",
        args.slug,
        target_slug,
        chapters.len(),
        args.chapters_dir,
        args.context_file.as_deref().unwrap_or("(none)"),
        args.model,
        args.max_turns,
    );

    let estimate_usd = estimate_cost(chapters.len(), &args.model);
    eprint!(
        "  est. cost:   ~${:.2} ({} subagents)\n\n",
        estimate_usd,
        chapters.len()
    );

    if args.dry_run {
        eprintln!("zbrain book-mirror: --dry-run — exiting without submission.");
        return Ok(());
    }

    if !args.no_confirm && !confirm_interactive(estimate_usd, chapters.len()) {
        eprintln!("zbrain book-mirror: cancelled by user.");
        return Ok(());
    }

    // Build the chat provider once from the model id (provider:model). Real API
    // key is read from env; this fails clearly when unset.
    let (parsed, recipe) = resolve_recipe_strict(&args.model)
        .map_err(|e| anyhow::anyhow!("zbrain book-mirror: {}", e.message))?;
    let provider = instantiate_chat(recipe, &parsed.model_id, |k| std::env::var(k).ok())
        .map_err(|e| anyhow::anyhow!("zbrain book-mirror: {e}"))?;

    let plan = BookMirrorPlan {
        slug: args.slug.clone(),
        title: book_title,
        author: args.author.clone(),
        context_pack,
        model: args.model.clone(),
        max_turns: args.max_turns,
        timeout_ms: args.timeout_ms,
        target_slug,
        concurrency: DEFAULT_WORKERS,
        today: today_stamp(),
    };

    eprintln!(
        "waiting for all {} chapters to complete...",
        chapters.len()
    );

    match orchestrate(Arc::clone(&engine), Arc::from(provider), &chapters, &plan).await? {
        Some(outcome) => {
            eprintln!(
                "\nassembled: {} chapters successful, {} failed.",
                outcome.chapters_completed, outcome.chapters_failed
            );
            eprintln!(
                "wrote: {} ({} chapter sections, {} bytes)",
                outcome.target_slug, outcome.chapters_total, outcome.bytes_written
            );
            println!(
                "{}",
                json!({
                    "slug": outcome.target_slug,
                    "chapters_total": outcome.chapters_total,
                    "chapters_completed": outcome.chapters_completed,
                    "chapters_failed": outcome.chapters_failed,
                })
            );
            if outcome.chapters_failed > 0 {
                eprintln!(
                    "\nzbrain book-mirror: {} chapter(s) failed. The page was written with the completed chapters; run again to retry (idempotency keys dedupe successful chapters).",
                    outcome.chapters_failed
                );
                std::process::exit(1);
            }
        }
        None => {
            anyhow::bail!(
                "zbrain book-mirror: every chapter failed. Not writing the brain page. Re-run after diagnosing."
            );
        }
    }

    Ok(())
}

/// `YYYY-MM-DD` in UTC for the frontmatter `date`.
fn today_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use zbrain_core::ai::chat::{
        ChatBlock, ChatError, ChatOpts, ChatProvider, ChatResult, ChatUsage, StopReason,
    };
    use zbrain_core::engine::{BrainEngine, GetPageOpts};
    use zbrain_core::InMemoryEngine;

    /// Stub provider: returns fixed markdown text as a single End turn, so the
    /// subagent tool-loop completes after one round without a real LLM.
    #[derive(Debug)]
    struct StubProvider {
        text: String,
    }
    impl StubProvider {
        fn new(t: &str) -> Self {
            Self { text: t.to_string() }
        }
    }
    #[async_trait]
    impl ChatProvider for StubProvider {
        async fn chat(&self, _opts: ChatOpts) -> std::result::Result<ChatResult, ChatError> {
            Ok(ChatResult {
                text: self.text.clone(),
                blocks: vec![ChatBlock::Text { text: self.text.clone() }],
                stop_reason: StopReason::End,
                usage: ChatUsage::default(),
                model: "mock:mock".to_string(),
                provider_id: "mock".to_string(),
                provider_metadata: None,
            })
        }
    }

    fn chapter(index: usize, name: &str, text: &str) -> ChapterEntry {
        ChapterEntry {
            index,
            filename: name.to_string(),
            text: text.to_string(),
            word_count: text.split_whitespace().count(),
        }
    }

    #[test]
    fn estimate_cost_opus_vs_sonnet() {
        assert!((estimate_cost(20, "anthropic:claude-opus-4-7") - 6.0).abs() < 1e-9);
        assert!((estimate_cost(20, "anthropic:claude-sonnet-4-5") - 1.2).abs() < 1e-9);
    }

    #[test]
    fn valid_slug_rules() {
        assert!(is_valid_slug("atomic-habits"));
        assert!(is_valid_slug("book2"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("-leading"));
        assert!(!is_valid_slug("has space"));
        assert!(!is_valid_slug("slash/slug"));
    }

    #[test]
    fn load_chapters_sorts_filters_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("02.txt"), "two words here").unwrap();
        std::fs::write(dir.path().join("01.txt"), "one").unwrap();
        std::fs::write(dir.path().join("notes.md"), "ignored non-txt").unwrap();
        let chapters = load_chapters(dir.path()).unwrap();
        assert_eq!(chapters.len(), 2, "only .txt counted");
        assert_eq!(chapters[0].filename, "01.txt");
        assert_eq!(chapters[0].index, 1);
        assert_eq!(chapters[1].filename, "02.txt");
        assert_eq!(chapters[1].word_count, 3);
    }

    #[test]
    fn load_chapters_errors_on_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_chapters(dir.path()).unwrap_err();
        assert!(err.contains("No .txt files"), "got: {err}");
    }

    #[test]
    fn build_chapter_prompt_embeds_text_and_context() {
        let ch = chapter(2, "02.txt", "chapter body text");
        let p = build_chapter_prompt(&ch, 5, "Deep Work", Some("Cal Newport"), Some("USER likes focus"));
        assert!(p.contains("chapter 2 of 5"));
        assert!(p.contains("\"Deep Work\" by Cal Newport"));
        assert!(p.contains("chapter body text"));
        assert!(p.contains("USER likes focus"));
        assert!(p.contains("read-only tools (get_page, search)"));
    }

    #[test]
    fn build_assembled_page_has_frontmatter_sorts_and_reports_failures() {
        let analyses = vec![
            ChapterAnalysis { index: 2, result: "## Chapter 2: B".into(), failed: false, error: None },
            ChapterAnalysis { index: 1, result: "## Chapter 1: A".into(), failed: false, error: None },
            ChapterAnalysis { index: 3, result: String::new(), failed: true, error: Some("timeout".into()) },
        ];
        let page = build_assembled_page(&AssembleOpts {
            title: "Deep Work",
            author: Some("Cal Newport"),
            context_pack: None,
            chapter_analyses: &analyses,
            today: "2026-07-24".into(),
        });
        assert!(page.contains("type: book-analysis"));
        assert!(page.contains("author: \"Cal Newport\""));
        assert!(page.contains("date: 2026-07-24"));
        let a = page.find("## Chapter 1: A").unwrap();
        let b = page.find("## Chapter 2: B").unwrap();
        assert!(a < b, "completed chapters sorted ascending by index");
        assert!(page.contains("Failed chapters (1)"));
        assert!(page.contains("Chapter 3: analysis failed (timeout)"));
    }

    /// Full submit→execute (inline worker)→assemble→put_page path with an
    /// in-memory engine and a stub provider — no real LLM, per the accepted
    /// "结构移植 + 单测/smoke" verification bar.
    #[tokio::test]
    async fn orchestrate_fans_out_executes_and_writes_page() {
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let provider: Arc<dyn ChatProvider> =
            Arc::new(StubProvider::new("## Chapter N: stub\n\n| L | R |\n|---|---|\n| a | b |"));
        let chapters = vec![
            chapter(1, "01.txt", "first chapter"),
            chapter(2, "02.txt", "second chapter"),
        ];
        let plan = BookMirrorPlan {
            slug: "test-book".into(),
            title: "Test Book".into(),
            author: None,
            context_pack: None,
            model: "anthropic:claude-opus-4-7".into(),
            max_turns: 3,
            timeout_ms: Some(5_000),
            target_slug: "media/books/test-book-personalized".into(),
            concurrency: 2,
            today: "2026-07-24".into(),
        };

        let outcome = orchestrate(Arc::clone(&engine), provider, &chapters, &plan)
            .await
            .expect("orchestrate ok")
            .expect("at least one chapter completed");
        assert_eq!(outcome.chapters_total, 2);
        assert_eq!(outcome.chapters_completed, 2);
        assert_eq!(outcome.chapters_failed, 0);
        assert!(outcome.bytes_written > 0);

        // Page persisted via operator-trust put_page.
        let page = engine
            .get_page(
                "media/books/test-book-personalized",
                &GetPageOpts { source_id: Some("default".into()), include_deleted: false },
            )
            .await
            .unwrap()
            .expect("page written");
        assert_eq!(page.title, "Test Book — Personalized");
        assert!(page.compiled_truth.contains("## Chapter N: stub"));
        assert!(page.compiled_truth.contains("type: book-analysis"));
    }
}
