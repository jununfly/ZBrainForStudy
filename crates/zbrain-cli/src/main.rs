//! Binary entry point for the `zbrain` CLI.
//!
//! Slice 1 prints the banner and exits successfully. Real subcommands arrive
//! in slice 8 via clap.

fn main() {
    println!("{}", zbrain_cli::banner());
}
