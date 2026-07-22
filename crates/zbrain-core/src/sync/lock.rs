//! Advisory cross-process file lock for `sync_brain` (roadmap 1-6-7-13 Q4).
//!
//! Prevents two concurrent `sync_brain` runs on the same source/repo from
//! interleaving their ingest loops. Implemented as a std-only advisory lock
//! file `<repo>/.zbrain-sync.lock` holding the holder pid + unix-epoch seconds.
//!
//! Design notes (why this and not the alternatives the grill considered):
//! - **Cross-process mutual exclusion** comes from the atomic
//!   `OpenOptions::new().create_new(true)` open: only one process can create
//!   the file; every other contender observes `AlreadyExists`. No in-memory
//!   `Mutex` (which can't lock across processes) is used.
//! - **No DB flag** (the other rejected alternative): a DB row needs manual
//!   stale-lock cleanup. Here, crash residue auto-recovers via a TTL check
//!   (30 min, matching TS `LOCK_TTL_MS`): a lock file older than the TTL is
//!   treated as stale and auto-broken on the next acquire. This mirrors TS
//!   `tryAcquireDbLock` + `formatLockBusyMessage` staleness semantics without
//!   adding a dependency or a cleanup job.
//! - **Zero new crates.** The grill floated `fd-lock`, but a pid-file lock
//!   needs nothing beyond `std`, and std-only keeps the slice dependency-free
//!   (consistent with the zero-new-dep git shell-out chosen in Q2).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Name of the advisory lock file placed in the repo root.
pub const SYNC_LOCK_FILE: &str = ".zbrain-sync.lock";

/// Lock time-to-live. A lock file older than this is treated as stale and
/// auto-broken on the next acquire (mirrors TS 30-minute `LOCK_TTL_MS`).
pub const SYNC_LOCK_TTL: Duration = Duration::from_secs(30 * 60);

/// Error returned when the sync lock is already held by another run.
#[derive(Debug)]
pub struct LockBusy {
    pub holder_pid: u32,
    pub acquired_at_epoch_secs: u64,
    pub lock_path: PathBuf,
}

impl std::fmt::Display for LockBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "another sync_brain is already in progress (lock {} held by pid {} since epoch {})",
            self.lock_path.display(),
            self.holder_pid,
            self.acquired_at_epoch_secs,
        )
    }
}

/// Failure to acquire the sync lock.
#[derive(Debug)]
pub enum AcquireSyncLockError {
    /// Another sync holds the lock (or a not-yet-stale residue remains).
    Busy(LockBusy),
    /// Underlying I/O failure (permissions, filesystem).
    Io(std::io::Error),
}

impl From<std::io::Error> for AcquireSyncLockError {
    fn from(e: std::io::Error) -> Self {
        AcquireSyncLockError::Io(e)
    }
}

/// An acquired advisory sync lock. Dropping it releases the lock by deleting
/// the lock file. While alive, no other `sync_brain` on the same repo can
/// acquire.
#[derive(Debug)]
pub struct SyncLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        // Best-effort: remove the lock file. Failure (e.g. already gone) is
        // non-fatal — the TTL guard covers stuck residue on later runs.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Atomically create the lock file with `pid\t<epoch-secs>\n` content.
/// Returns `AlreadyExists` if the file is already present.
fn create_lock_file(path: &Path) -> std::io::Result<SyncLock> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    writeln!(file, "{}\t{}", std::process::id(), now_epoch_secs())?;
    Ok(SyncLock {
        _file: file,
        path: path.to_path_buf(),
    })
}

/// Parse the `(pid, epoch_secs)` holder tuple written by `create_lock_file`.
fn read_lock_holder(path: &Path) -> Option<(u32, u64)> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut parts = content.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let secs = parts.next()?.parse::<u64>().ok()?;
    Some((pid, secs))
}

/// Try to acquire the advisory sync lock for `repo`.
///
/// Returns `Ok(SyncLock)` on success (drop to release). Returns
/// `Err(AcquireSyncLockError::Busy)` if another live (or not-yet-stale) sync
/// holds it. Stale locks (older than [`SYNC_LOCK_TTL`]) are auto-broken and
/// re-acquired once.
pub fn acquire_sync_lock(repo: &Path) -> Result<SyncLock, AcquireSyncLockError> {
    let path = repo.join(SYNC_LOCK_FILE);

    match create_lock_file(&path) {
        Ok(lock) => return Ok(lock),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Contended, or a stale residue from a crashed run. Inspect below.
        }
        Err(e) => return Err(AcquireSyncLockError::Io(e)),
    }

    if let Some((pid, secs)) = read_lock_holder(&path) {
        let age = now_epoch_secs().saturating_sub(secs);
        if age > SYNC_LOCK_TTL.as_secs() {
            // Stale: auto-break and re-acquire once.
            let _ = std::fs::remove_file(&path);
            return match create_lock_file(&path) {
                Ok(lock) => Ok(lock),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    Err(AcquireSyncLockError::Busy(LockBusy {
                        holder_pid: pid,
                        acquired_at_epoch_secs: secs,
                        lock_path: path,
                    }))
                }
                Err(e) => Err(AcquireSyncLockError::Io(e)),
            };
        }
        // Live (or unparseable-but-present): refuse.
        return Err(AcquireSyncLockError::Busy(LockBusy {
            holder_pid: pid,
            acquired_at_epoch_secs: secs,
            lock_path: path,
        }));
    }

    // File exists but is unreadable/empty → treat as busy to be safe.
    Err(AcquireSyncLockError::Busy(LockBusy {
        holder_pid: 0,
        acquired_at_epoch_secs: 0,
        lock_path: path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn acquires_and_releases_lock_file() {
        let tmp = std::env::temp_dir().join(format!("zbrain_lock_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let lock_path = tmp.join(SYNC_LOCK_FILE);
        let _ = fs::remove_file(&lock_path);

        let guard1 = acquire_sync_lock(&tmp).expect("first acquire should succeed");
        assert!(lock_path.exists(), "lock file should exist while held");

        // Second acquire must refuse while the first is alive.
        let second = acquire_sync_lock(&tmp);
        assert!(
            matches!(second, Err(AcquireSyncLockError::Busy(_))),
            "second acquire should be busy: {:?}",
            second
        );

        drop(guard1);
        assert!(
            !lock_path.exists(),
            "lock file should be removed after drop"
        );

        // Now re-acquirable.
        let _guard3 = acquire_sync_lock(&tmp).expect("re-acquirable after release");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn busy_error_carries_holder_info() {
        let tmp = std::env::temp_dir().join(format!("zbrain_lock_busy_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let lock_path = tmp.join(SYNC_LOCK_FILE);
        let _ = fs::remove_file(&lock_path);

        let _g = acquire_sync_lock(&tmp).unwrap();
        let err = acquire_sync_lock(&tmp).unwrap_err();
        match err {
            AcquireSyncLockError::Busy(b) => {
                assert!(b.holder_pid > 0, "busy error should report a real pid");
                assert!(!b.lock_path.as_os_str().is_empty());
            }
            AcquireSyncLockError::Io(_) => panic!("expected Busy, got Io"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }
}
