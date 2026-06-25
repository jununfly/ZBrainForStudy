//! Binary entry point for the `zbrain` CLI.
//!
//! Slice 1-3-1: clap CLI framework with 4 command stubs.

use clap::Parser;
use zbrain_cli::{run, Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run(cli).await?;
    Ok(())
}
