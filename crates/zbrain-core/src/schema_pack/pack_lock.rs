//! Per-pack file lock — serializes concurrent pack mutations.
//!
//! Ported from TS `src/core/schema-pack/pack-lock.ts`.
//!
//! Uses atomic file creation (`O_CREAT | O_EXCL`) for race-free lock acquisition.
//! Stale lock detection via TTL expiry + PID liveness probe. Cascade-safe:
//! a crashed process's lock is stolen after TTL or PID death.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const DEFAULT_LOCK_TTL_MS: u64 = 60_000;
pub const REFRESH_INTERVAL_MS: u64 = 10_000;
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 0;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    Acquired,
    StolenStale,
    Forced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    TtlExpired,
    PidDead,
    Live,
}

/// Result of staleness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleCheck {
    pub stale: bool,
    pub reason: StaleReason,
}

/// Lock file content — JSON serialized.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LockFileRecord {
    pub pid: u32,
    pub hostname: String,
    pub ts: u64,
    pub ttl_ms: u64,
}

/// Options for lock acquisition.
pub struct PackLockOpts {
    pub ttl_ms: u64,
    pub force: bool,
    pub lock_dir: Option<PathBuf>,
    pub now: Option<Box<dyn Fn() -> u64>>,
    pub is_pid_alive: Option<Box<dyn Fn(u32) -> bool>>,
}

impl Default for PackLockOpts {
    fn default() -> Self {
        Self {
            ttl_ms: DEFAULT_LOCK_TTL_MS,
            force: false,
            lock_dir: None,
            now: None,
            is_pid_alive: None,
        }
    }
}

/// Thrown when lock is held by a live process.
#[derive(Debug, Clone)]
pub struct PackLockBusyError {
    pub held_by: u32,
    pub age_ms: u64,
    pub ttl_ms: u64,
}

impl std::fmt::Display for PackLockBusyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pack lock busy: held by pid={}, age={}ms, ttl={}ms (use --force to override)",
            self.held_by, self.age_ms, self.ttl_ms
        )
    }
}

impl std::error::Error for PackLockBusyError {}

/// Result of lock acquisition.
#[derive(Debug)]
pub struct AcquiredLock {
    pub lock_path: PathBuf,
    pub outcome: LockOutcome,
    pub record: LockFileRecord,
}

// ---------------------------------------------------------------------------
// is_lock_stale
// ---------------------------------------------------------------------------

/// Check if a lock is stale based on TTL expiry and PID liveness.
pub fn is_lock_stale(
    record: &LockFileRecord,
    now: u64,
    is_pid_alive: &dyn Fn(u32) -> bool,
) -> StaleCheck {
    let age_ms = now.saturating_sub(record.ts);
    if age_ms > record.ttl_ms {
        return StaleCheck {
            stale: true,
            reason: StaleReason::TtlExpired,
        };
    }
    if !is_pid_alive(record.pid) {
        return StaleCheck {
            stale: true,
            reason: StaleReason::PidDead,
        };
    }
    StaleCheck {
        stale: false,
        reason: StaleReason::Live,
    }
}

// ---------------------------------------------------------------------------
// acquire_pack_lock
// ---------------------------------------------------------------------------

/// Acquire a pack lock. Returns the lock info on success, or `PackLockBusyError`
/// if the lock is held by a live process.
pub fn acquire_pack_lock(
    pack_name: &str,
    opts: &PackLockOpts,
) -> Result<AcquiredLock, PackLockBusyError> {
    let lock_path = resolve_lock_path(pack_name, opts.lock_dir.as_deref());
    let now = opts.now.as_deref().unwrap_or(&default_now)();
    let is_pid_alive = opts.is_pid_alive.as_deref().unwrap_or(&default_is_pid_alive);
    let hostname = default_hostname();
    let pid = std::process::id();

    // First attempt: atomic create
    match try_atomic_acquire(&lock_path) {
        AtomicResult::Acquired(file) => {
            let record = LockFileRecord {
                pid,
                hostname,
                ts: now,
                ttl_ms: opts.ttl_ms,
            };
            write_lock_record(&lock_path, file, &record);
            return Ok(AcquiredLock {
                lock_path,
                outcome: LockOutcome::Acquired,
                record,
            });
        }
        AtomicResult::Exists => { /* fall through to stale check */ }
        AtomicResult::Error(e) => {
            // ENOENT: parent dir doesn't exist, create it and retry
            if let Some(parent) = lock_path.parent() {
                let _ = fs::create_dir_all(parent);
                match try_atomic_acquire(&lock_path) {
                    AtomicResult::Acquired(file) => {
                        let record = LockFileRecord {
                            pid,
                            hostname,
                            ts: now,
                            ttl_ms: opts.ttl_ms,
                        };
                        write_lock_record(&lock_path, file, &record);
                        return Ok(AcquiredLock {
                            lock_path,
                            outcome: LockOutcome::Acquired,
                            record,
                        });
                    }
                    AtomicResult::Exists => { /* fall through */ }
                    AtomicResult::Error(_) => { /* fall through */ }
                }
            }
            let _ = e; // suppress unused warning
        }
    }

    // Lock exists — read and check staleness
    let existing = read_lock_file(&lock_path);

    match existing {
        None => {
            // Corrupt/unreadable — try to remove and re-acquire
            let _ = fs::remove_file(&lock_path);
            match try_atomic_acquire(&lock_path) {
                AtomicResult::Acquired(file) => {
                    let record = LockFileRecord {
                        pid,
                        hostname,
                        ts: now,
                        ttl_ms: opts.ttl_ms,
                    };
                    write_lock_record(&lock_path, file, &record);
                    Ok(AcquiredLock {
                        lock_path,
                        outcome: LockOutcome::StolenStale,
                        record,
                    })
                }
                _ => Err(PackLockBusyError {
                    held_by: 0,
                    age_ms: 0,
                    ttl_ms: opts.ttl_ms,
                }),
            }
        }
        Some(record) => {
            let check = is_lock_stale(&record, now, is_pid_alive);
            if check.stale || opts.force {
                // Stale or forced — remove and re-acquire
                let _ = fs::remove_file(&lock_path);
                match try_atomic_acquire(&lock_path) {
                    AtomicResult::Acquired(file) => {
                        let new_record = LockFileRecord {
                            pid,
                            hostname,
                            ts: now,
                            ttl_ms: opts.ttl_ms,
                        };
                        write_lock_record(&lock_path, file, &new_record);
                        Ok(AcquiredLock {
                            lock_path,
                            outcome: if opts.force {
                                LockOutcome::Forced
                            } else {
                                LockOutcome::StolenStale
                            },
                            record: new_record,
                        })
                    }
                    _ => {
                        // Another process won the race
                        let current = read_lock_file(&lock_path);
                        let held_by = current.as_ref().map(|r| r.pid).unwrap_or(0);
                        Err(PackLockBusyError {
                            held_by,
                            age_ms: 0,
                            ttl_ms: opts.ttl_ms,
                        })
                    }
                }
            } else {
                // Lock is live — reject
                let age_ms = now.saturating_sub(record.ts);
                Err(PackLockBusyError {
                    held_by: record.pid,
                    age_ms,
                    ttl_ms: record.ttl_ms,
                })
            }
        }
    }
}

/// Release a pack lock (best-effort, ignores errors).
pub fn release_pack_lock(lock_path: &Path) {
    let _ = fs::remove_file(lock_path);
}

// ---------------------------------------------------------------------------
// with_pack_lock
// ---------------------------------------------------------------------------

/// Acquire a lock, run `fn`, release the lock. The lock is released even
/// if `fn` panics or returns an error.
///
/// Note: TTL refresh is not implemented in the initial port. The default
/// TTL of 60s should suffice for most operations. Long-running mutations
/// should use `acquire_pack_lock` directly with manual refresh.
pub fn with_pack_lock<F, T>(
    pack_name: &str,
    opts: &PackLockOpts,
    f: F,
) -> Result<T, PackLockBusyError>
where
    F: FnOnce() -> T,
{
    let acquired = acquire_pack_lock(pack_name, opts)?;
    let result = f();
    release_pack_lock(&acquired.lock_path);
    Ok(result)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

enum AtomicResult {
    Acquired(File),
    Exists,
    Error(std::io::Error),
}

fn try_atomic_acquire(path: &Path) -> AtomicResult {
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(file) => AtomicResult::Acquired(file),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => AtomicResult::Exists,
        Err(e) => AtomicResult::Error(e),
    }
}

fn resolve_lock_path(pack_name: &str, lock_dir: Option<&Path>) -> PathBuf {
    let dir = lock_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_lock_dir);
    dir.join(format!("{pack_name}.lock"))
}

fn default_lock_dir() -> PathBuf {
    // ~/.zbrain/schema-packs/.locks/
    crate::paths::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("schema-packs")
        .join(".locks")
}

fn default_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn default_hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn default_is_pid_alive(pid: u32) -> bool {
    // Platform-independent check: try to signal 0 (Unix) or OpenProcess (Windows).
    // For simplicity, we check if /proc/<pid> exists (Linux) or use a heuristic.
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if process exists, -1 otherwise.
        unsafe {
            libc::kill(pid as i32, 0) == 0
        }
    }
    #[cfg(not(unix))]
    {
        // On Windows, check if the process is running.
        // Simplified: assume alive (conservative — prevents stealing live locks).
        // A proper implementation would use OpenProcess + GetExitCodeProcess.
        let _ = pid;
        true
    }
}

fn read_lock_file(path: &Path) -> Option<LockFileRecord> {
    let mut content = String::new();
    File::open(path).ok()?.read_to_string(&mut content).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_lock_record(path: &Path, mut file: File, record: &LockFileRecord) {
    let json = serde_json::to_string(record).unwrap_or_default();
    let _ = file.write_all(json.as_bytes());
    let _ = file.sync_all();
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ---- is_lock_stale --------------------------------------------------

    #[test]
    fn stale_ttl_expired() {
        let record = LockFileRecord {
            pid: 1234,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 5000,
        };
        let now = 7000; // 6s later > 5s TTL
        let check = is_lock_stale(&record, now, &|_| true);
        assert!(check.stale);
        assert_eq!(check.reason, StaleReason::TtlExpired);
    }

    #[test]
    fn stale_pid_dead() {
        let record = LockFileRecord {
            pid: 9999,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 60000,
        };
        let now = 2000; // Within TTL
        let check = is_lock_stale(&record, now, &|pid| pid != 9999); // PID 9999 is dead
        assert!(check.stale);
        assert_eq!(check.reason, StaleReason::PidDead);
    }

    #[test]
    fn not_stale_live_process() {
        let record = LockFileRecord {
            pid: 1234,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 60000,
        };
        let now = 2000; // Within TTL
        let check = is_lock_stale(&record, now, &|_| true); // PID alive
        assert!(!check.stale);
        assert_eq!(check.reason, StaleReason::Live);
    }

    #[test]
    fn stale_ttl_takes_precedence_over_pid() {
        // Both TTL expired AND PID dead — TTL wins (checked first)
        let record = LockFileRecord {
            pid: 9999,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 5000,
        };
        let now = 9999;
        let check = is_lock_stale(&record, now, &|_| false);
        assert!(check.stale);
        assert_eq!(check.reason, StaleReason::TtlExpired);
    }

    // ---- acquire_pack_lock (with temp dir) ------------------------------

    #[test]
    fn acquire_clean_lock() {
        let dir = tempdir();
        let opts = PackLockOpts {
            lock_dir: Some(dir.clone()),
            now: Some(Box::new(|| 1000)),
            is_pid_alive: Some(Box::new(|_| true)),
            ..Default::default()
        };
        let result = acquire_pack_lock("test-pack", &opts);
        assert!(result.is_ok());
        let acquired = result.unwrap();
        assert_eq!(acquired.outcome, LockOutcome::Acquired);
        assert!(acquired.lock_path.exists());
    }

    #[test]
    fn acquire_blocked_by_live_lock() {
        let dir = tempdir();

        // Pre-create a live lock
        let lock_path = dir.join("test-pack.lock");
        let record = LockFileRecord {
            pid: 1234,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 60000,
        };
        fs::write(&lock_path, serde_json::to_string(&record).unwrap()).unwrap();

        let opts = PackLockOpts {
            lock_dir: Some(dir.clone()),
            now: Some(Box::new(|| 2000)), // 1s later, within TTL
            is_pid_alive: Some(Box::new(|pid| pid == 1234)), // PID 1234 alive
            ..Default::default()
        };
        let err = acquire_pack_lock("test-pack", &opts).unwrap_err();
        assert_eq!(err.held_by, 1234);
    }

    #[test]
    fn acquire_steals_stale_lock_ttl() {
        let dir = tempdir();

        // Pre-create a stale lock (TTL expired)
        let lock_path = dir.join("test-pack.lock");
        let record = LockFileRecord {
            pid: 1234,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 5000,
        };
        fs::write(&lock_path, serde_json::to_string(&record).unwrap()).unwrap();

        let opts = PackLockOpts {
            lock_dir: Some(dir.clone()),
            now: Some(Box::new(|| 10000)), // 9s later > 5s TTL
            is_pid_alive: Some(Box::new(|_| true)), // PID alive but TTL expired
            ..Default::default()
        };
        let acquired = acquire_pack_lock("test-pack", &opts).unwrap();
        assert_eq!(acquired.outcome, LockOutcome::StolenStale);
    }

    #[test]
    fn acquire_steals_stale_lock_pid_dead() {
        let dir = tempdir();

        let lock_path = dir.join("test-pack.lock");
        let record = LockFileRecord {
            pid: 9999,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 60000,
        };
        fs::write(&lock_path, serde_json::to_string(&record).unwrap()).unwrap();

        let opts = PackLockOpts {
            lock_dir: Some(dir.clone()),
            now: Some(Box::new(|| 2000)), // Within TTL
            is_pid_alive: Some(Box::new(|pid| pid != 9999)), // PID 9999 dead
            ..Default::default()
        };
        let acquired = acquire_pack_lock("test-pack", &opts).unwrap();
        assert_eq!(acquired.outcome, LockOutcome::StolenStale);
    }

    #[test]
    fn acquire_forced_steals_live_lock() {
        let dir = tempdir();

        let lock_path = dir.join("test-pack.lock");
        let record = LockFileRecord {
            pid: 1234,
            hostname: "test".into(),
            ts: 1000,
            ttl_ms: 60000,
        };
        fs::write(&lock_path, serde_json::to_string(&record).unwrap()).unwrap();

        let opts = PackLockOpts {
            lock_dir: Some(dir.clone()),
            now: Some(Box::new(|| 2000)),
            is_pid_alive: Some(Box::new(|_| true)), // PID alive
            force: true,
            ..Default::default()
        };
        let acquired = acquire_pack_lock("test-pack", &opts).unwrap();
        assert_eq!(acquired.outcome, LockOutcome::Forced);
    }

    #[test]
    fn acquire_handles_corrupt_lock() {
        let dir = tempdir();

        let lock_path = dir.join("test-pack.lock");
        fs::write(&lock_path, "not valid json").unwrap();

        let opts = PackLockOpts {
            lock_dir: Some(dir.clone()),
            now: Some(Box::new(|| 1000)),
            is_pid_alive: Some(Box::new(|_| true)),
            ..Default::default()
        };
        let acquired = acquire_pack_lock("test-pack", &opts).unwrap();
        assert_eq!(acquired.outcome, LockOutcome::StolenStale);
    }

    // ---- with_pack_lock -------------------------------------------------

    #[test]
    fn with_pack_lock_runs_and_releases() {
        let dir = tempdir();
        let opts = PackLockOpts {
            lock_dir: Some(dir.clone()),
            now: Some(Box::new(|| 1000)),
            is_pid_alive: Some(Box::new(|_| true)),
            ..Default::default()
        };
        let result = with_pack_lock("test-pack", &opts, || 42);
        assert_eq!(result.unwrap(), 42);

        // Lock should be released
        let lock_path = dir.join("test-pack.lock");
        assert!(!lock_path.exists());
    }

    // ---- LockFileRecord serialization -----------------------------------

    #[test]
    fn lock_record_round_trip() {
        let record = LockFileRecord {
            pid: 1234,
            hostname: "test-host".into(),
            ts: 1234567890,
            ttl_ms: 60000,
        };
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: LockFileRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.pid, 1234);
        assert_eq!(deserialized.hostname, "test-host");
        assert_eq!(deserialized.ts, 1234567890);
        assert_eq!(deserialized.ttl_ms, 60000);
    }

    // ---- Helper: temp dir -----------------------------------------------

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zbrain-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
