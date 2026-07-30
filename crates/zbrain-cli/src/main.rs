//! Binary entry point for the `zbrain` CLI.
//!
//! Slice 1-3-1: clap CLI framework with 4 command stubs.

use clap::Parser;
use zbrain_cli::{run, Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The clap command tree is huge; on Windows the default 1 MB stack
    // overflows during `Cli::parse()` (construction, before any command
    // logic runs). Parse on a thread with a larger stack to avoid the
    // `--help`/`--version`/parse stack overflow. Harmless on other platforms.
    let cli = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| Cli::parse())
        .expect("failed to spawn parse thread")
        .join()
        .expect("parse thread panicked");
    run(cli).await?;
    Ok(())
}
