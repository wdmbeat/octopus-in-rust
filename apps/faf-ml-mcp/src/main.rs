//! MCP server (stdio) exposing the faf-ml platform workflow as LLM tools.
//!
//! Thin wrapper over the faf-ml-server REST/WS API — see `server.rs` for the
//! tool surface. Configure the API base with `FAF_ML_API` (default
//! `http://localhost:3100`).

mod api;
mod server;
mod training;

use rmcp::{transport::stdio, ServiceExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs must go to stderr — stdout is the MCP channel. Default to `warn`
    // so rmcp's INFO handshake chatter doesn't pollute the agent's TUI;
    // set RUST_LOG=info to see it while debugging.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let service = server::FafMl::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
