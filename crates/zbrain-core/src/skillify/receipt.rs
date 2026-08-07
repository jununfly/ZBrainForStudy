//! Minimal cross-modal-eval receipt lookup for the `skillify check` audit
//! (item 11, informational). Ported from `src/core/cross-modal-eval/receipt-name.ts`.
//!
//! Pure read-side helpers — no filesystem writes. The full cross-modal-eval
//! runner is a separate (later) port; this module only covers the receipt
//! binding used by the audit so the `#11` item can report found / stale /
//! missing against receipts already on disk.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// SHA-256 of skill content, truncated to 8 hex chars. Mirrors TS `sha8`.
pub fn sha8(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    hex::encode(h.finalize())[..8].to_string()
}

/// Status of a receipt lookup for a (slug, content) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptStatus {
    Found {
        path: PathBuf,
        sha: String,
    },
    Stale {
        latest_path: PathBuf,
        latest_sha: String,
        current_sha: String,
    },
    Missing {
        current_sha: String,
    },
}

/// Infer the slug from a SKILL.md path: the immediate parent directory name.
/// Returns `None` when the path does not end in `SKILL.md` or has no parent.
pub fn infer_slug_from_skill_path(skill_md_path: &Path) -> Option<String> {
    let normalized = skill_md_path.to_string_lossy().replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    let n = parts.len();
    if n < 2 {
        return None;
    }
    if parts[n - 1] != "SKILL.md" {
        return None;
    }
    Some(parts[n - 2].to_string())
}

/// Look up the cross-modal eval receipt for a SKILL.md + receipt directory.
///
/// Mirrors TS `findReceiptForSkill`: when the SKILL.md is missing/unreadable
/// this returns `Missing` with an empty sha (the audit treats that as
/// informational, never a hard failure). Receipt filenames embed the sha-8 of
/// the SKILL.md content, so a matching filename means the receipt is current.
pub fn find_receipt_for_skill(skill_md_path: &Path, receipt_dir: &Path) -> ReceiptStatus {
    let content = match fs::read_to_string(skill_md_path) {
        Ok(c) => c,
        Err(_) => return ReceiptStatus::Missing {
            current_sha: String::new(),
        },
    };
    let slug = match infer_slug_from_skill_path(skill_md_path) {
        Some(s) => s,
        None => return ReceiptStatus::Missing {
            current_sha: String::new(),
        },
    };
    let current_sha = sha8(&content);
    let expected_name = format!("{}-{}.json", slug, current_sha);
    let expected_path = receipt_dir.join(&expected_name);
    if expected_path.exists() {
        return ReceiptStatus::Found {
            path: expected_path,
            sha: current_sha,
        };
    }
    if !receipt_dir.exists() {
        return ReceiptStatus::Missing { current_sha };
    }

    // Look for stale receipts (same slug, different sha).
    let prefix = format!("{}-", slug);
    let mut matches: Vec<(PathBuf, String, std::time::SystemTime)> = Vec::new();
    if let Ok(entries) = fs::read_dir(receipt_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) || !name.ends_with(".json") {
                continue;
            }
            let sha = name[prefix.len()..name.len() - ".json".len()].to_string();
            if sha == current_sha || !is_sha8(&sha) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                matches.push((
                    entry.path(),
                    sha,
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                ));
            }
        }
    }
    if matches.is_empty() {
        return ReceiptStatus::Missing { current_sha };
    }
    matches.sort_by(|a, b| b.2.cmp(&a.2));
    let latest = &matches[0];
    ReceiptStatus::Stale {
        latest_path: latest.0.clone(),
        latest_sha: latest.1.clone(),
        current_sha,
    }
}

fn is_sha8(s: &str) -> bool {
    s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Human-readable description of a receipt status. Mirrors TS
/// `describeReceiptStatus` (error strings only differ in phrasing).
pub fn describe_receipt_status(slug: &str, status: &ReceiptStatus) -> String {
    match status {
        ReceiptStatus::Found { sha, .. } => format!(
            "cross-modal eval receipt found for {} (sha {}; matches current SKILL.md)",
            slug, sha
        ),
        ReceiptStatus::Stale {
            latest_sha,
            current_sha,
            ..
        } => format!(
            "cross-modal eval receipt for {} exists for an older SKILL.md \
             (receipt sha {}, current sha {}). Re-run `zbrain eval cross-modal` \
             against the current skill output.",
            slug, latest_sha, current_sha
        ),
        ReceiptStatus::Missing { .. } => format!(
            "no cross-modal eval receipt for {} yet — run `zbrain eval cross-modal` to add one",
            slug
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn sha8_is_8_hex_chars() {
        let h = sha8("hello world");
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        // deterministic
        assert_eq!(h, sha8("hello world"));
    }

    #[test]
    fn infer_slug_pulls_parent_dir() {
        assert_eq!(
            infer_slug_from_skill_path(Path::new("skills/foo/SKILL.md")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            infer_slug_from_skill_path(Path::new("a/b/skills/bar/SKILL.md")).as_deref(),
            Some("bar")
        );
        assert_eq!(infer_slug_from_skill_path(Path::new("notskill.md")), None);
    }

    #[test]
    fn found_when_receipt_matches_current_sha() {
        let tmp = std::env::temp_dir().join(format!("zb_rcpt_found_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // SKILL.md under skills/foo/ so the inferred slug is "foo".
        fs::create_dir_all(tmp.join("skills/foo")).unwrap();
        let skill = write_skill(&tmp.join("skills/foo"), "SKILL.md", "body of the skill");
        let sha = sha8("body of the skill");
        // Receipt lives in the (home) eval-receipts dir — here `tmp`.
        write_skill(&tmp, &format!("foo-{}.json", sha), "{}");
        let status = find_receipt_for_skill(&skill, &tmp);
        assert!(matches!(status, ReceiptStatus::Found { .. }));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_when_no_receipt_dir() {
        let tmp = std::env::temp_dir().join(format!("zb_rcpt_miss_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let skill = write_skill(&tmp, "SKILL.md", "body");
        let status = find_receipt_for_skill(&skill, &tmp.join("no-such-dir"));
        assert!(matches!(status, ReceiptStatus::Missing { .. }));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stale_when_only_older_receipt_exists() {
        let tmp = std::env::temp_dir().join(format!("zb_rcpt_stale_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir_all(tmp.join("skills/foo")).unwrap();
        let skill = write_skill(&tmp.join("skills/foo"), "SKILL.md", "body v2");
        // an older receipt for a different sha
        write_skill(&tmp, "foo-deadbeef.json", "{}");
        let status = find_receipt_for_skill(&skill, &tmp);
        assert!(matches!(status, ReceiptStatus::Stale { .. }));
        let _ = fs::remove_dir_all(&tmp);
    }
}
