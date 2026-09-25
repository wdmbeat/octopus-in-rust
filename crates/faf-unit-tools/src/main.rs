//! faf-unit-tools — CLI utilities for FAF unit data.
//!
//! Subcommands:
//!   download  — download and persist the FAF unit database (former faf-downloader)
//!   icon-map  — cross-check strategic-icon sprites against the unit database

mod download;
mod icon_map;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "faf-unit-tools", about = "FAF unit data utilities.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Download and persist FAF unit data (JSON or SQLite).
    Download(download::DownloadArgs),
    /// Cross-check strategic icon sprites against the unit database.
    IconMap(icon_map::IconMapArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Download(args) => download::run(args).await,
        Command::IconMap(args) => icon_map::run(args),
    }
}
