use anyhow::{Context, Result};

use crate::cargo;

/// Run a faf-ml-specific command (the FAF unit-detection ML platform).
pub fn run(command: &str, rest: &[String]) -> Result<()> {
    match command {
        "backend" => run_backend(),
        "frontend" => run_frontend(),
        "build-web" => build_web(rest),
        "mcp" => run_mcp(),
        "help" | "-h" | "--help" => {
            crate::args::print_faf_ml_help();
            Ok(())
        }
        other => {
            eprintln!("Unknown faf-ml command '{}'", other);
            crate::args::print_faf_ml_help();
            std::process::exit(1);
        }
    }
}

/// Start the Axum backend (serves the API on :3100 and, if built, the
/// release web UI at the same port).
fn run_backend() -> Result<()> {
    let mut cmd = cargo::command();
    cmd.args(["run", "--package", "faf-ml-server"]);

    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env("RUST_LOG", "info,faf_ml_server=debug,tower_http=info");
    }

    println!("Starting faf-ml backend on http://localhost:3100 ...");
    let mut child = cmd.spawn().context("failed to spawn faf-ml-server")?;
    println!("Server PID: {}", child.id());

    let status = child.wait().context("failed to wait for faf-ml-server")?;
    if !status.success() {
        anyhow::bail!("faf-ml-server exited with status: {status}");
    }
    Ok(())
}

/// Start the Dioxus dev server with hot reload (debug builds call the
/// backend on localhost:3100).
fn run_frontend() -> Result<()> {
    let mut cmd = std::process::Command::new("dx");
    cmd.args(["serve", "--platform", "web", "--port", "8081"]);
    cmd.current_dir("apps/faf-ml-web");

    println!("Starting faf-ml frontend (hot reload)...");
    let status = cmd.status().context("failed to run dx serve")?;
    if !status.success() {
        anyhow::bail!("dx serve exited with status: {status}");
    }
    Ok(())
}

/// Build the MCP server, then launch `kimi` with the faf-ml tools scoped to
/// that session only (`--mcp-config`) — the global ~/.kimi/mcp.json is NOT
/// touched. Requires the backend on :3100 (`cargo xtask faf-ml backend`).
fn run_mcp() -> Result<()> {
    let mut build = cargo::command();
    build.args(["build", "--package", "faf-ml-mcp"]);
    println!("Building faf-ml-mcp ...");
    let status = build.status().context("failed to build faf-ml-mcp")?;
    if !status.success() {
        anyhow::bail!("cargo build -p faf-ml-mcp exited with status: {status}");
    }

    let binary = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/faf-ml-mcp")
        .canonicalize()
        .context("locating the faf-ml-mcp binary")?;
    let mcp_config = serde_json::json!({
        "mcpServers": {
            "faf-ml": { "command": binary }
        }
    });

    println!(
        "Starting kimi with the faf-ml MCP tools (session-scoped; \\\n         backend must be running on :3100) ..."
    );
    let mut cmd = std::process::Command::new("kimi");
    cmd.arg("--mcp-config")
        .arg(serde_json::to_string(&mcp_config)?);
    let status = cmd.status().context("failed to run kimi")?;
    if !status.success() {
        anyhow::bail!("kimi exited with status: {status}");
    }
    Ok(())
}

/// Build the web UI. Release is the default because the backend serves the
/// release bundle (target/dx/faf-ml-web/release/web/public).
fn build_web(rest: &[String]) -> Result<()> {
    let debug = rest.iter().any(|a| a == "--debug");
    let mut cmd = std::process::Command::new("dx");
    cmd.args(["build", "--platform", "web"]);
    if !debug {
        cmd.arg("--release");
    }
    cmd.current_dir("apps/faf-ml-web");

    println!(
        "Building faf-ml-web ({})...",
        if debug { "debug" } else { "release" }
    );
    let status = cmd.status().context("failed to run dx build")?;
    if !status.success() {
        anyhow::bail!("dx build exited with status: {status}");
    }
    Ok(())
}
