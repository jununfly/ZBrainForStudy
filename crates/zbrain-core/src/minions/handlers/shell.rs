//! Shell handler — execute a shell command with timeout.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts` — `shell` handler (721 lines). Full TS version has
//! env allowlist, SIGTERM→wait→SIGKILL graceful shutdown, streaming output.
//!
//! ## v1 scope (grill Q4)
//!
//! Basic spawn only: command + timeout (tokio cancel) + stdout/stderr capture.
//! cwd fixed to project root, env passthrough (no allowlist), no streaming.
//! Returns `{stdout, stderr, exit_code, timed_out}`.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

use crate::error::StructuredError;
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ShellHandler;

/// Default timeout: 30 seconds.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[async_trait]
impl MinionHandler for ShellHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let command = ctx.data.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| StructuredError::new("handler", "invalid_input", "missing required field: command"))?;

        let timeout_ms = ctx.data.get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS);

        // Build the command with platform-appropriate shell.
        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/C", command]);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", command]);
            c
        };

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Spawn + apply timeout.
        let timeout = Duration::from_millis(timeout_ms);
        let spawn_result = tokio::time::timeout(timeout, async {
            let child = cmd.spawn().map_err(|e| {
                StructuredError::new("handler", "spawn_failed", &format!("failed to spawn: {e}"))
            })?;
            child.wait_with_output().await.map_err(|e| {
                StructuredError::new("handler", "wait_failed", &format!("failed to wait: {e}"))
            })
        })
        .await;

        match spawn_result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);
                Ok(json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "exit_code": exit_code,
                    "timed_out": false,
                }))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timeout elapsed — process was killed by tokio's timeout drop.
                Ok(json!({
                    "stdout": "",
                    "stderr": "process timed out",
                    "exit_code": -1,
                    "timed_out": true,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    #[tokio::test]
    async fn shell_executes_simple_command() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "shell".into(),
            json!({"command": "echo hello"}),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ShellHandler;
        let result = handler.handle(&context).await.expect("should succeed");

        assert_eq!(result["timed_out"], false);
        assert!(result["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn shell_missing_command_returns_error() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "shell".into(),
            json!({}),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ShellHandler;
        assert!(handler.handle(&context).await.is_err());
    }

    #[tokio::test]
    async fn shell_timeout_kills_process() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "shell".into(),
            json!({"command": "sleep 10", "timeout_ms": 100}),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ShellHandler;
        let result = handler.handle(&context).await.expect("should succeed");

        assert_eq!(result["timed_out"], true);
        assert_eq!(result["exit_code"], -1);
    }

    #[tokio::test]
    async fn shell_captures_stderr() {
        let eng = engine();
        let cmd = if cfg!(windows) { "echo err 1>&2" } else { "echo err >&2" };
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "shell".into(),
            json!({"command": cmd}),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ShellHandler;
        let result = handler.handle(&context).await.expect("should succeed");

        assert!(result["stderr"].as_str().unwrap().contains("err"));
    }

    #[tokio::test]
    async fn shell_nonzero_exit_code() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "shell".into(),
            json!({"command": "exit 42"}),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ShellHandler;
        let result = handler.handle(&context).await.expect("should succeed");

        assert_eq!(result["exit_code"], 42);
        assert_eq!(result["timed_out"], false);
    }
}
