//! Sync failure recording and acknowledgement.
//!
//! Failures are recorded as JSONL (one JSON object per line) in
//! `~/.zbrain/failures/<source_id>.jsonl`. Each line is a
//! `SyncFailure` serialized as JSON.
//!
//! The `acknowledge` operation renames the file to `<source_id>.jsonl.ack`,
//! which effectively clears the failure list for that source.

use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// A single sync failure record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncFailure {
    /// The relative path of the file that failed to sync.
    pub path: String,
    /// The error message describing what went wrong.
    pub error: String,
    /// ISO 8601 timestamp of when the failure was recorded.
    pub recorded_at: String,
    /// Whether this failure has been acknowledged.
    #[serde(default)]
    pub acknowledged: bool,
}

/// Record sync failures by appending them to the JSONL file for the given source.
///
/// Creates the failures directory if it doesn't exist.
pub async fn record_sync_failures(
    source_id: &str,
    failures: &[SyncFailure],
    failures_dir: &Path,
) -> Result<(), std::io::Error> {
    if failures.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(failures_dir).await?;

    let file_path = failures_dir.join(format!("{source_id}.jsonl"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .await?;

    for failure in failures {
        let mut line = serde_json::to_string(failure)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
    }

    file.flush().await?;
    Ok(())
}

/// Acknowledge all failures for a source by renaming the failures file.
///
/// After acknowledgement, the file is renamed from `<source_id>.jsonl`
/// to `<source_id>.jsonl.ack`, which means `list_unacknowledged_failures`
/// will return an empty list for this source.
pub async fn acknowledge_failures(
    source_id: &str,
    failures_dir: &Path,
) -> Result<(), std::io::Error> {
    let file_path = failures_dir.join(format!("{source_id}.jsonl"));
    let ack_path = failures_dir.join(format!("{source_id}.jsonl.ack"));

    match fs::rename(&file_path, &ack_path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// List unacknowledged failures for a source.
///
/// Returns an empty `Vec` if there are no failures or if the failures
/// have been acknowledged (file renamed to `.ack`).
pub async fn list_unacknowledged_failures(
    source_id: &str,
    failures_dir: &Path,
) -> Result<Vec<SyncFailure>, std::io::Error> {
    let file_path = failures_dir.join(format!("{source_id}.jsonl"));

    let content = match fs::read_to_string(&file_path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut failures = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let failure: SyncFailure = serde_json::from_str(line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        failures.push(failure);
    }

    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_failure(path: &str, error: &str) -> SyncFailure {
        SyncFailure {
            path: path.to_string(),
            error: error.to_string(),
            recorded_at: "2026-07-01T12:00:00Z".to_string(),
            acknowledged: false,
        }
    }

    #[tokio::test]
    async fn record_and_list_failures() {
        let dir = TempDir::new().unwrap();
        let failures_dir = dir.path().join("failures");
        let source_id = "test-source-1";

        let failures = vec![
            make_failure("docs/a.md", "parse error: invalid frontmatter"),
            make_failure("docs/b.md", "io error: permission denied"),
        ];

        // Record
        record_sync_failures(source_id, &failures, &failures_dir)
            .await
            .unwrap();

        // List
        let listed = list_unacknowledged_failures(source_id, &failures_dir)
            .await
            .unwrap();

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].path, "docs/a.md");
        assert_eq!(listed[1].path, "docs/b.md");
    }

    #[tokio::test]
    async fn acknowledge_clears_failures() {
        let dir = TempDir::new().unwrap();
        let failures_dir = dir.path().join("failures");
        let source_id = "test-source-1";

        let failures = vec![make_failure("docs/x.md", "timeout")];
        record_sync_failures(source_id, &failures, &failures_dir)
            .await
            .unwrap();

        // Before acknowledge: 1 failure
        assert_eq!(
            list_unacknowledged_failures(source_id, &failures_dir)
                .await
                .unwrap()
                .len(),
            1
        );

        // Acknowledge
        acknowledge_failures(source_id, &failures_dir)
            .await
            .unwrap();

        // After acknowledge: 0 failures
        assert_eq!(
            list_unacknowledged_failures(source_id, &failures_dir)
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn record_appends_to_existing_file() {
        let dir = TempDir::new().unwrap();
        let failures_dir = dir.path().join("failures");
        let source_id = "test-source-1";

        // First batch
        let batch1 = vec![make_failure("a.md", "err1")];
        record_sync_failures(source_id, &batch1, &failures_dir)
            .await
            .unwrap();

        // Second batch
        let batch2 = vec![make_failure("b.md", "err2")];
        record_sync_failures(source_id, &batch2, &failures_dir)
            .await
            .unwrap();

        let listed = list_unacknowledged_failures(source_id, &failures_dir)
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].path, "a.md");
        assert_eq!(listed[1].path, "b.md");
    }

    #[tokio::test]
    async fn list_empty_for_nonexistent_source() {
        let dir = TempDir::new().unwrap();
        let failures_dir = dir.path().join("failures");

        let listed = list_unacknowledged_failures("no-such-source", &failures_dir)
            .await
            .unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn acknowledge_nonexistent_is_noop() {
        let dir = TempDir::new().unwrap();
        let failures_dir = dir.path().join("failures");

        // Should not panic or error
        acknowledge_failures("no-such-source", &failures_dir)
            .await
            .unwrap();
    }
}
