//! Filesystem-backed run store for `brainstorm` / `lsd` results (1-1-5-9).
//!
//! Mirrors the `suspected-contradictions` trend store but lives on the local
//! filesystem (not the engine DB), so runs persist independently of the brain
//! DB and are trivially trendable / reviewable / resumable. Each run is a
//! single `<run_id>.json` file holding the full `BrainstormResult` (as JSON)
//! plus a `schema_version` + `saved_at` envelope.
//!
//! The on-disk `result` is stored as `serde_json::Value`, but since
//! `BrainstormResult` is now `Serialize + Deserialize`, `load_run_result`
//! can rebuild the full typed result for `--review-run` (and 1-1-5-11 resume)
//! by deserializing the JSON. `list_runs` projects a small summary out of the
//! JSON for the trend table.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::checkpoint::CURRENT_SCHEMA;
use super::orchestrator::BrainstormResult;

/// One persisted run row on disk: schema version + wall-clock timestamp +
/// the full result payload (as JSON). The schema version lets trend/review
/// tooling reject incompatible old payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainstormRunRow {
    pub schema_version: u16,
    /// RFC3339 timestamp of the save.
    pub saved_at: String,
    /// The full `BrainstormResult`, stored opaquely as JSON.
    pub result: serde_json::Value,
}

/// A lightweight summary for `--list-runs` (no full idea bodies).
#[derive(Debug, Clone, Serialize)]
pub struct BrainstormRunSummary {
    pub run_id: String,
    pub schema_version: u16,
    pub saved_at: String,
    pub profile_label: String,
    pub question: String,
    pub n_ideas: usize,
    pub n_passed: usize,
    /// Mean `concrete_grounding` (1-5) across judged ideas; `None` when no
    /// idea was judged (e.g. judge_failed) — the second trend axis (Q2-A).
    pub mean_grounding: Option<f64>,
    pub actual_usd: f64,
    pub judge_failed: bool,
}

/// Default store dir under the zbrain home, honoring `ZBRAIN_HOME`. Parallel
/// to the default `sqlite://~/.zbrain/zbrain.db` location.
#[must_use]
pub fn default_store_dir() -> PathBuf {
    crate::paths::zbrain_home()
        .unwrap_or_else(|| PathBuf::from(".zbrain"))
        .join("runs")
        .join("brainstorm")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Persist a run. Atomic: write `<run_id>.json.tmp` then rename. Returns the
/// final path. `run_id` comes from `result.run_id` (the stable sha256, Q3-A5).
pub fn save_run(result: &BrainstormResult, store_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(store_dir)
        .with_context(|| format!("create brainstorm run store dir {}", store_dir.display()))?;
    let row = BrainstormRunRow {
        schema_version: CURRENT_SCHEMA,
        saved_at: crate::time::current_utc_iso8601(),
        result: serde_json::to_value(result)
            .context("serialize BrainstormResult for run store")?,
    };
    let path = store_dir.join(format!("{}.json", result.run_id));
    let tmp = store_dir.join(format!("{}.json.tmp", result.run_id));
    let bytes = serde_json::to_vec_pretty(&row).context("encode run row")?;
    std::fs::write(&tmp, &bytes).with_context(|| format!("write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(path)
}

/// True if `path` looks like a run-store JSON file (extension `json`, not a
/// `.tmp` temp file).
fn is_run_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("json")
        && !path.to_string_lossy().contains(".tmp")
}

/// List run summaries, newest first. Tolerates corrupt rows (skipped with a
/// stderr warning) so a single bad file can't break `--list-runs`.
#[must_use]
pub fn list_runs(store_dir: &Path) -> Vec<BrainstormRunSummary> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(store_dir) {
        Ok(e) => e,
        Err(_) => return out, // no dir yet → empty list
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_run_file(&path) {
            continue;
        }
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "zbrain brainstorm: skipping unreadable run file {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        let row: BrainstormRunRow = match serde_json::from_slice(&data) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "zbrain brainstorm: skipping corrupt run file {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        out.push(summarize(&row));
    }
    out.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    out
}

/// Project a [`BrainstormRunRow`] (with opaque JSON result) into a summary.
#[must_use]
pub fn summarize(row: &BrainstormRunRow) -> BrainstormRunSummary {
    let r = &row.result;
    let ideas = r.get("ideas").and_then(|v| v.as_array());
    let n_ideas = ideas.map_or(0, Vec::len);
    let n_passed = ideas
        .map(|a| a.iter().filter(|i| i.get("passes").and_then(|v| v.as_bool()) == Some(true)).count())
        .unwrap_or(0);
    // Mean grounding (Q2-A): average `judge.scores.concrete_grounding` across
    // ideas that actually carry a judge verdict. `judge_failed` runs (judge is
    // null) and unjudged ideas are skipped; if none, None.
    let mean_grounding = ideas.and_then(|a| {
        let vals: Vec<f64> = a
            .iter()
            .filter_map(|i| {
                i.get("judge")
                    .and_then(|j| j.get("scores"))
                    .and_then(|s| s.get("concrete_grounding"))
                    .and_then(|v| v.as_f64())
            })
            .collect();
        if vals.is_empty() {
            None
        } else {
            Some(vals.iter().sum::<f64>() / vals.len() as f64)
        }
    });
    BrainstormRunSummary {
        run_id: r.get("run_id").and_then(|v| v.as_str()).unwrap_or("<unknown>").to_string(),
        schema_version: row.schema_version,
        saved_at: row.saved_at.clone(),
        profile_label: r
            .get("profile_label")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),
        question: r.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        n_ideas,
        n_passed,
        mean_grounding,
        actual_usd: r
            .get("cost")
            .and_then(|c| c.get("actual_usd"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        judge_failed: r.get("judge_failed").and_then(|v| v.as_bool()).unwrap_or(false),
    }
}

/// Reclaim runs older than `max_age_days` (mtime-based). Returns the count
/// reclaimed. Corrupt / tmp files are left alone.
#[must_use]
pub fn gc_stale_runs(store_dir: &Path, max_age_days: u64) -> u64 {
    gc_stale_runs_inner(store_dir, max_age_days, now_ms())
}

/// Inner GC with an injectable `now_ms` (for tests). Public(crate) so the
/// test module can drive deterministic reclaim without backdating file mtimes.
pub(crate) fn gc_stale_runs_inner(store_dir: &Path, max_age_days: u64, now_ms: u64) -> u64 {
    let max_age_ms = max_age_days.saturating_mul(86_400_000u64);
    let mut reclaimed = 0u64;
    let entries = match std::fs::read_dir(store_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_run_file(&path) {
            continue;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now_ms.saturating_sub(mtime_ms) > max_age_ms {
            if std::fs::remove_file(&path).is_ok() {
                reclaimed += 1;
            }
        }
    }
    reclaimed
}

/// Delete a single run by id. Returns true if a file was removed.
#[must_use]
pub fn clear_run(store_dir: &Path, run_id: &str) -> bool {
    let path = store_dir.join(format!("{run_id}.json"));
    std::fs::remove_file(&path).is_ok()
}

/// Load a single run row by id (used by resume at 1-1-5-11).
#[must_use]
pub fn load_run(store_dir: &Path, run_id: &str) -> Option<BrainstormRunRow> {
    let path = store_dir.join(format!("{run_id}.json"));
    let data = std::fs::read(&path).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Rebuild the full typed `BrainstormResult` from a persisted run (1-1-5-10
/// `--review-run`, and 1-1-5-11 resume). Returns None if the row is missing or
/// the JSON no longer deserializes (schema drift).
#[must_use]
pub fn load_run_result(store_dir: &Path, run_id: &str) -> Option<BrainstormResult> {
    let row = load_run(store_dir, run_id)?;
    serde_json::from_value(row.result).ok()
}

/// Filter run summaries to those within the last `days` (by `saved_at`,
/// RFC3339). `days == 0` returns all runs. Newest-first order from
/// `list_runs` is preserved. Unparseable timestamps are kept (not dropped).
#[must_use]
pub fn recent_runs_by_days(runs: &[BrainstormRunSummary], days: u64) -> Vec<BrainstormRunSummary> {
    if days == 0 {
        return runs.to_vec();
    }
    let cutoff = chrono::Utc::now()
        .date_naive()
        .checked_sub_days(chrono::Days::new(days))
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    runs.iter()
        .filter(|r| {
            chrono::DateTime::parse_from_rfc3339(&r.saved_at)
                .map(|dt| dt.date_naive() >= cutoff)
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Render the brainstorm run trend as a fixed-width ASCII table (faithful to
/// the contradictions `render_trend_chart` style). Two axes (Q2-A): pass-rate%
/// and mean grounding (1-5). Newest-first order is expected from the caller.
pub fn render_trend_chart(rows: &[BrainstormRunSummary]) -> String {
    if rows.is_empty() {
        return "No brainstorm runs recorded yet. Run `zbrain brainstorm` (or `zbrain eval-brainstorm`) first.".to_string();
    }
    let mut out = String::new();
    out.push_str("Date        Profile    Ideas Pass  Rate%  Gnd   PassBar    GndBar\n");
    out.push_str("----------  ---------  ----- ----- ------ ----- ---------- ----------\n");
    for r in rows {
        let date = r.saved_at.get(0..10).unwrap_or(&r.saved_at); // YYYY-MM-DD
        let profile = trunc_display(&r.profile_label, 10);
        let pass_rate = if r.n_ideas > 0 {
            r.n_passed as f64 / r.n_ideas as f64 * 100.0
        } else {
            0.0
        };
        let rate = format!("{pass_rate:>5.1}%");
        let gnd = match r.mean_grounding {
            Some(g) => format!("{g:>5.1}"),
            None => "    -".to_string(),
        };
        let pass_len = ((pass_rate / 100.0) * 10.0).round() as usize;
        let pass_bar = "#".repeat(pass_len.min(10));
        let gnd_bar = match r.mean_grounding {
            Some(g) => {
                let gnd_len = ((g / 5.0) * 10.0).round() as usize;
                "=".repeat(gnd_len.min(10))
            }
            None => String::new(),
        };
        out.push_str(&format!(
            "{date:<10}  {profile:<10}  {:>5} {:>5} {rate:>6} {gnd:>5} {pass_bar:<10} {gnd_bar}\n",
            r.n_ideas, r.n_passed
        ));
    }
    out
}

/// Render the metadata header for a single reviewed run (Q3-A): run_id,
/// saved_at, profile, question, idea/pass counts, pass-rate, mean grounding,
/// cost, and judge status. The full idea report is produced separately by
/// `format_brainstorm_markdown` over the rebuilt `BrainstormResult`.
pub fn render_review_header(row: &BrainstormRunRow) -> String {
    let r = &row.result;
    let run_id = r.get("run_id").and_then(|v| v.as_str()).unwrap_or("<unknown>");
    let profile = r.get("profile_label").and_then(|v| v.as_str()).unwrap_or("?");
    let question = r.get("question").and_then(|v| v.as_str()).unwrap_or("");
    let ideas = r.get("ideas").and_then(|v| v.as_array()).map_or(0, Vec::len);
    let n_passed = r
        .get("ideas")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter(|i| i.get("passes").and_then(|v| v.as_bool()) == Some(true))
                .count()
        })
        .unwrap_or(0);
    let rate = if ideas > 0 {
        n_passed as f64 / ideas as f64 * 100.0
    } else {
        0.0
    };
    let gnd = summarize(row).mean_grounding;
    let cost = r
        .get("cost")
        .and_then(|c| c.get("actual_usd"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let judge_failed = r.get("judge_failed").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut out = String::new();
    out.push_str(&format!("Run:        {run_id}\n"));
    out.push_str(&format!("Saved:      {}\n", row.saved_at));
    out.push_str(&format!("Profile:    {profile}\n"));
    out.push_str(&format!("Question:   {question}\n"));
    out.push_str(&format!("Ideas:      {ideas}  Passed: {n_passed}  ({rate:.1}%)\n"));
    out.push_str(&format!(
        "Grounding:  {}\n",
        gnd.map_or_else(|| "-".to_string(), |g| format!("{g:.2} / 5"))
    ));
    out.push_str(&format!("Cost:       ${cost:.2}\n"));
    out.push_str(&format!(
        "Judge:      {}\n",
        if judge_failed { "FAILED" } else { "ok" }
    ));
    out
}

/// Right-truncate a string to `max` chars for fixed-width display.
fn trunc_display(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for ch in s.chars() {
        if n >= max {
            break;
        }
        out.push(ch);
        n += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::brainstorm::judges::{JudgeAxisScores, JudgeIdeaResult};
    use crate::eval::brainstorm::orchestrator::{
        BrainstormCost, BrainstormIdea, BrainstormResult, CloseRefForReport, FarRefForReport,
    };

    fn tmp_store() -> PathBuf {
        std::env::temp_dir().join(format!("bs_store_{}", std::process::id()))
    }

    /// Build a minimal `BrainstormResult` (the struct is Serialize-only, so we
    /// only need enough fields for the summary projection).
    fn mk_result(run_id: &str, n_ideas: usize, n_passed: usize) -> BrainstormResult {
        let mut ideas = Vec::new();
        for i in 0..n_ideas {
            ideas.push(BrainstormIdea {
                id: format!("{:02}", i + 1),
                text: format!("idea {i}"),
                close_slug: "a".to_string(),
                far_slug: "b".to_string(),
                distance_score: 0.5,
                judge: None,
                passes: i < n_passed,
                judge_failed: false,
            });
        }
        BrainstormResult {
            profile_label: "brainstorm".to_string(),
            question: "why do tools converge?".to_string(),
            embedding_model: None,
            ideas,
            close_set: vec![CloseRefForReport { slug: "a".to_string(), title: None }],
            far_set: vec![FarRefForReport {
                slug: "b".to_string(),
                title: None,
                distance_score: 0.5,
                source: "prefix-stratified".to_string(),
            }],
            active_bias_tags: None,
            short_of_target: false,
            judge_failed: false,
            cost: BrainstormCost {
                estimated_usd: 0.1,
                actual_usd: 0.07,
                input_tokens: 100,
                output_tokens: 50,
            },
            run_id: run_id.to_string(),
        }
    }

    #[test]
    fn save_and_list_roundtrip() {
        let dir = tmp_store();
        std::fs::create_dir_all(&dir).ok();
        // Clean slate.
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_file(e.path());
        }

        let result = mk_result("deadbeefcafe1234", 5, 3);
        let path = save_run(&result, &dir).unwrap();
        assert!(path.exists());
        assert!(!path.to_string_lossy().contains(".tmp"));

        let runs = list_runs(&dir);
        assert_eq!(runs.len(), 1);
        let s = &runs[0];
        assert_eq!(s.run_id, "deadbeefcafe1234");
        assert_eq!(s.n_ideas, 5);
        assert_eq!(s.n_passed, 3);
        assert!((s.actual_usd - 0.07).abs() < 1e-9);
        assert_eq!(s.profile_label, "brainstorm");
        assert_eq!(s.question, "why do tools converge?");
    }

    #[test]
    fn list_is_empty_before_any_save() {
        let dir = tmp_store().join("empty");
        std::fs::create_dir_all(&dir).ok();
        assert!(list_runs(&dir).is_empty());
    }

    #[test]
    fn gc_reclaims_old_files_only() {
        let dir = tmp_store().join("gc");
        std::fs::create_dir_all(&dir).ok();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_file(e.path());
        }

        save_run(&mk_result("aaaaaaaaaaaaaaaa", 2, 1), &dir).unwrap();
        save_run(&mk_result("bbbbbbbbbbbbbbbb", 2, 1), &dir).unwrap();

        // Fresh mtime → with real `now`, a 7-day window reclaims nothing.
        assert_eq!(gc_stale_runs(&dir, 7), 0);
        assert_eq!(list_runs(&dir).len(), 2);

        // Inject a future `now` so both files look older than 7 days → both reclaimed.
        let future = now_ms() + 8 * 86_400_000;
        assert_eq!(gc_stale_runs_inner(&dir, 7, future), 2);
        assert!(list_runs(&dir).is_empty());
    }

    #[test]
    fn clear_run_removes_by_id() {
        let dir = tmp_store().join("clear");
        std::fs::create_dir_all(&dir).ok();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_file(e.path());
        }
        let rid = "cccccccccccccccc";
        save_run(&mk_result(rid, 1, 0), &dir).unwrap();
        assert!(clear_run(&dir, rid));
        assert!(!clear_run(&dir, rid)); // idempotent miss
        assert!(load_run(&dir, rid).is_none());
    }

    #[test]
    fn load_run_roundtrip() {
        let dir = tmp_store().join("load");
        std::fs::create_dir_all(&dir).ok();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_file(e.path());
        }
        let result = mk_result("dddddddddddddddd", 4, 2);
        save_run(&result, &dir).unwrap();
        let row = load_run(&dir, "dddddddddddddddd").expect("row present");
        assert_eq!(row.schema_version, CURRENT_SCHEMA);
        assert_eq!(row.result.get("run_id").and_then(|v| v.as_str()), Some("dddddddddddddddd"));
        // summary projection off the loaded row matches.
        let s = summarize(&row);
        assert_eq!(s.n_ideas, 4);
        assert_eq!(s.n_passed, 2);
    }

    #[test]
    fn summarize_mean_grounding_and_load_result() {
        let dir = tmp_store().join("gnd");
        std::fs::create_dir_all(&dir).ok();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_file(e.path());
        }
        // Two judged ideas (grounding 3.0 and 5.0) → mean 4.0.
        let mut result = mk_result("feedfacefeedface", 2, 2);
        result.ideas[0].judge = Some(JudgeIdeaResult {
            id: "01".to_string(),
            scores: JudgeAxisScores {
                originality: 4.0,
                resistance: 4.0,
                thesis_density: 4.0,
                concrete_grounding: 3.0,
                cognitive_load: 4.0,
            },
            weighted_score: 4.0,
            passes: true,
            note: "ok".to_string(),
        });
        result.ideas[1].judge = Some(JudgeIdeaResult {
            id: "02".to_string(),
            scores: JudgeAxisScores {
                originality: 4.0,
                resistance: 4.0,
                thesis_density: 4.0,
                concrete_grounding: 5.0,
                cognitive_load: 4.0,
            },
            weighted_score: 4.0,
            passes: true,
            note: "ok".to_string(),
        });
        save_run(&result, &dir).unwrap();

        let runs = list_runs(&dir);
        let s = &runs[0];
        assert_eq!(s.n_ideas, 2);
        assert_eq!(s.n_passed, 2);
        assert!((s.mean_grounding.unwrap() - 4.0).abs() < 1e-9);

        // load_run_result rebuilds the typed BrainstormResult (incl. judges).
        let loaded = load_run_result(&dir, "feedfacefeedface").expect("rebuild");
        assert_eq!(loaded.profile_label, "brainstorm");
        assert_eq!(loaded.ideas.len(), 2);
        assert!(
            (loaded.ideas[0].judge.as_ref().unwrap().scores.concrete_grounding - 3.0).abs() < 1e-9
        );

        // The trend chart renders the row (profile column present).
        let chart = render_trend_chart(&runs);
        assert!(chart.contains("brainstorm"));
    }

    #[test]
    fn corrupt_files_are_skipped() {
        let dir = tmp_store().join("corrupt");
        std::fs::create_dir_all(&dir).ok();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_file(e.path());
        }
        save_run(&mk_result("eeeeeeeeeeeeeeee", 1, 1), &dir).unwrap();
        // Drop a garbage file in the same dir.
        std::fs::write(dir.join("garbage.json"), "not json{").ok();
        let runs = list_runs(&dir);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "eeeeeeeeeeeeeeee");
    }
}
