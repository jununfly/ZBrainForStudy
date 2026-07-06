//! Local read-only wall-clock timeout helper.
//!
//! Ports the live part of the TS `src/core/timeout.ts` + `cli.ts:1125-1170`
//! behavior: the read-only `sources list` command gets a wall-clock deadline
//! on both the connect phase and the command body so a hung schema probe /
//! frozen listing surfaces a timeout instead of spinning at 100% CPU (the
//! production "10-day zombie zbrain search" bug class). Note the TS
//! `search → 30s` sibling (cli.ts:1136) is dead code — `search`/`query` are
//! shared ops that never enter `handleCliOnly` — so only `sources list` is
//! ported here.
//!
//! Design:
//!   * `with_read_only_timeout` — pure wrapper over `tokio::time::timeout`.
//!     Returns `Ok(T)` on completion, `Err(ReadOnlyTimeout)` on deadline.
//!     Never touches the process (fully unit-testable), mirroring the TS
//!     `withTimeout` layering where `process.exit` lives in the CLI caller.
//!   * `format_timeout_message` — pure formatter for the stderr line, with a
//!     `(default Nms; pass --timeout=Ns to override)` hint only when the user
//!     did NOT supply `--timeout` (mirrors cli.ts:1151/1161).
//!   * `report_timeout_and_exit` — the single caller-side sink: prints the
//!     formatted message to stderr and exits 124 (GNU timeout convention).
//!
//! Unlike a true cancellation primitive, `tokio::time::timeout` drops the
//! wrapped future on expiry; for a CLI that immediately `exit(124)`s, process
//! teardown is the real resource-release mechanism (same reasoning as the TS
//! `withTimeout` doc comment).

/// A read-only command exceeded its wall-clock deadline.
///
/// Carries enough context for `format_timeout_message` to build the exact
/// user-facing line without the caller re-deriving anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyTimeout {
    /// Human label for the phase that timed out, e.g. `zbrain query` or
    /// `zbrain query: connect` (mirrors TS `zbrain <cmd>` / `: connect`).
    pub label: String,
    /// The deadline that was exceeded, in milliseconds.
    pub ms: u64,
    /// Whether the deadline came from a user-supplied `--timeout` (true) or a
    /// built-in per-command default (false). Controls the override hint.
    pub user_supplied: bool,
}

/// Format the stderr line for a read-only timeout.
///
/// Mirrors cli.ts:1150-1152 / 1160-1162:
///   * user-supplied `--timeout`: `"<label> timed out."`
///   * built-in default:          `"<label> timed out (default <ms>ms; pass --timeout=Ns to override)."`
#[must_use]
pub fn format_timeout_message(t: &ReadOnlyTimeout) -> String {
    if t.user_supplied {
        format!("{} timed out.", t.label)
    } else {
        format!(
            "{} timed out (default {}ms; pass --timeout=Ns to override).",
            t.label, t.ms
        )
    }
}

/// Race `fut` against a `timeout_ms` wall-clock deadline.
///
/// Returns `Ok(T)` if the future completes first, or `Err(ReadOnlyTimeout)`
/// (carrying `label` / `ms` / `user_supplied`) if the deadline expires. On
/// expiry, `tokio::time::timeout` drops `fut`; for a CLI that immediately
/// `exit(124)`s this is the intended resource-release path.
///
/// Mirrors the TS `withTimeout` wrapper: bounds USER wait, does not cancel
/// server-side work beyond dropping the local future.
pub async fn with_read_only_timeout<T, F>(
    fut: F,
    timeout_ms: u64,
    label: &str,
    user_supplied: bool,
) -> Result<T, ReadOnlyTimeout>
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), fut).await {
        Ok(v) => Ok(v),
        Err(_elapsed) => Err(ReadOnlyTimeout {
            label: label.to_string(),
            ms: timeout_ms,
            user_supplied,
        }),
    }
}

/// Print the timeout message to stderr and exit 124 (GNU `timeout` convention).
///
/// The single caller-side sink for a read-only wall-clock timeout. Mirrors the
/// TS `cli.ts` catch blocks (`console.error(...); process.exit(124)`); the
/// formatting itself lives in the unit-tested `format_timeout_message`, so this
/// wrapper stays trivial and its only untestable step is the `exit`.
pub fn report_timeout_and_exit(t: &ReadOnlyTimeout) -> ! {
    eprintln!("{}", format_timeout_message(t));
    std::process::exit(124)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_message_with_default_includes_override_hint() {
        let t = ReadOnlyTimeout {
            label: "zbrain query".to_string(),
            ms: 30_000,
            user_supplied: false,
        };
        assert_eq!(
            format_timeout_message(&t),
            "zbrain query timed out (default 30000ms; pass --timeout=Ns to override)."
        );
    }

    #[test]
    fn format_message_with_user_timeout_omits_hint() {
        let t = ReadOnlyTimeout {
            label: "zbrain query: connect".to_string(),
            ms: 5_000,
            user_supplied: true,
        };
        assert_eq!(
            format_timeout_message(&t),
            "zbrain query: connect timed out."
        );
    }

    #[tokio::test]
    async fn with_timeout_returns_ok_when_future_completes_in_time() {
        let out = with_read_only_timeout(async { 42u32 }, 1_000, "zbrain query", false).await;
        assert_eq!(out, Ok(42));
    }

    #[tokio::test]
    async fn with_timeout_returns_err_when_deadline_expires() {
        // Future sleeps well past the 20ms deadline → must time out.
        let out = with_read_only_timeout(
            async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                7u32
            },
            20,
            "zbrain query: connect",
            true,
        )
        .await;
        assert_eq!(
            out,
            Err(ReadOnlyTimeout {
                label: "zbrain query: connect".to_string(),
                ms: 20,
                user_supplied: true,
            })
        );
    }
}
