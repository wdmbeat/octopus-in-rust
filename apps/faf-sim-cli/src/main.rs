//! CLI for the FAF build-queue simulator and build-order scheduler.
//!
//! This binary dispatches to focused modules in `src/*.rs`. Most commands are
//! thin wrappers over the `faf-sim` and `faf-sim-service` crates.

mod build;
mod command_line;
mod util;

use clap::Parser;
use command_line::Cli;
use faf_blueprints::FafBlueprints;

use crate::command_line::Command::{Build, Search};
use anyhow::Result;

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Search { str } => {
            let faf = FafBlueprints::new()?;
            let info = faf.get_one_unit_from_search(&str)?;
            println!("{:?}", info);
        }
        // Run the construction plan through the Bevy-backed simulation service.
        // `speed` controls real-world playback; the engine itself is tick-based.
        Build {
            plan_file: queue,
            speed,
        } => {
            let construction_plan_str = std::fs::read_to_string(&queue).unwrap_or_else(|e| {
                eprintln!("Failed to read: {}, error: {}", queue.display(), e);
                std::process::exit(1);
            });

            build::run(&construction_plan_str, speed)?;
        }
    }
    Ok(())
}
