use anyhow::{bail, Result};

/// Top-level application selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Fafcn,
    FafSim,
    Qqbot,
    FafMl,
}

/// Commands that apply to the whole workspace rather than a single app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalCommand {
    Test,
}

/// Parsed command-line invocation.
#[derive(Debug, Clone)]
pub enum Task {
    App {
        app: App,
        command: String,
        rest: Vec<String>,
    },
    Global(GlobalCommand),
}

impl Task {
    pub fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let first = args.next().unwrap_or_else(|| "help".to_string());

        match first.as_str() {
            "fafcn" => Self::parse_app(App::Fafcn, args, "help"),
            "faf-sim" => Self::parse_app(App::FafSim, args, "run"),
            "qqbot" => Self::parse_app(App::Qqbot, args, "help"),
            "faf-ml" => Self::parse_app(App::FafMl, args, "help"),
            "test" => Ok(Task::Global(GlobalCommand::Test)),
            "help" | "-h" | "--help" => {
                print_top_help();
                std::process::exit(0);
            }
            other => {
                bail!("unknown command '{}'. Run `cargo xtask` for help.", other);
            }
        }
    }

    fn parse_app(
        app: App,
        mut args: impl Iterator<Item = String>,
        default_command: &str,
    ) -> Result<Task> {
        let command = args.next().unwrap_or_else(|| default_command.to_string());
        let rest: Vec<String> = args.collect();
        Ok(Task::App { app, command, rest })
    }
}

pub fn print_fafcn_help() {
    println!("cargo xtask fafcn — run the FAF construction simulator");
    println!();
    println!("Usage:");
    println!("  cargo xtask fafcn <command>");
    println!();
    println!("Commands:");
    println!("  backend    Start the Axum backend (cargo run --package fafcn-server)");
    println!(
        "             The server writes logs to data/logs/fafcn-server.log and prints them to the console"
    );
    println!("  frontend   Start the Dioxus dev server (dx serve --platform web)");
    println!("  file-sync  Cross-compile the fafcn-sync CLI for Windows and install");
    println!("             it under data/faf-gamedata/client/ so the /sync download");
    println!("             link serves players a real Windows binary");
    println!("             Options: --debug  Debug profile (release is the default;");
    println!("                      debug builds keep a console window on Windows)");
    println!("  unit-update  Refresh plugins/faf-units/data/faf_units.json from the");
    println!("             upstream unit database (faf-unit-tools download). Prints the");
    println!("             follow-up commands (rebuild plugin, deploy, qqbot).");
    println!("  majiko-deploy  Build and redeploy the whole stack to the majiko server");
    println!("             (8v.pub). Reads MAJIKO_* settings from xtask/.env (see");
    println!("             xtask/.env.example). Requires sshpass + rsync locally.");
    println!("             Options: --skip-web       backend/plugin only, keep web UI");
    println!("                      --with-gamedata  also sync the ~800MB mirror");
    println!("                      --skip-verify    skip post-deploy health gates");
    println!("  majiko-deploy-file-sync  Rebuild the fafcn-sync Windows client and");
    println!("             ship ONLY that binary to the majiko server (no full redeploy,");
    println!("             no service restart). Reads MAJIKO_* settings from xtask/.env.");
    println!("  majiko-health  Check the majiko deployment in three layers:");
    println!("             SSH login, service on the host (systemd + 127.0.0.1:3000),");
    println!("             and the public URL (MAJIKO_PUBLIC_URL). Exits non-zero if");
    println!("             any layer fails.");
    println!();
    println!("Examples:");
    println!("  cargo xtask fafcn backend");
    println!("  cargo xtask fafcn frontend");
    println!("  cargo xtask fafcn file-sync");
    println!("  cargo xtask fafcn majiko-deploy-file-sync");
    println!("  cargo xtask fafcn majiko-deploy");
    println!("  cargo xtask fafcn majiko-health");
}

pub fn print_faf_sim_help() {
    println!("cargo xtask faf-sim — run and serve the FAF eco/build simulator");
    println!();
    println!("Usage:");
    println!("  cargo xtask faf-sim [command] [options]");
    println!();
    println!("Commands:");
    println!("  run        Run the native simulator (default)");
    println!("  web        Build and serve the WASM bundle");
    println!();
    println!("Native options:");
    println!("  --release  Use the release profile");
    println!();
    println!("Web options:");
    println!("  build      Build the WASM binary and run wasm-bindgen");
    println!("  serve      Build the bundle and start the embedded Axum server (default)");
    println!("  --release  Use the release profile for all builds");
    println!("  --port N   Port for the Axum server (default: 8080)");
    println!();
    println!("Examples:");
    println!("  cargo xtask faf-sim");
    println!("  cargo xtask faf-sim --release");
    println!("  cargo xtask faf-sim web build --release");
    println!("  cargo xtask faf-sim web serve --port 3000");
}

pub fn print_faf_ml_help() {
    println!("cargo xtask faf-ml — FAF unit-detection ML platform");
    println!();
    println!("Usage:");
    println!("  cargo xtask faf-ml <command> [args]");
    println!();
    println!("Commands:");
    println!("  backend    Start the Axum backend on :3100 (cargo run -p faf-ml-server);");
    println!("             serves the release web build too (run build-web first)");
    println!("  frontend   Start the Dioxus dev server with hot reload on :8081");
    println!("             (dx serve; debug builds call the backend on localhost:3100)");
    println!("  build-web  Build the web UI (release by default — that's what the");
    println!("             backend serves). Options: --debug");
    println!("  mcp        Build the MCP server, then launch kimi with the faf-ml");
    println!("             tools session-scoped (--mcp-config; global config untouched).");
    println!("             Requires the backend on :3100 (cargo xtask faf-ml backend)");
    println!("             (datagen runs inside the server now — the web UI's Datagen");
    println!("             view is the generation flow; there is no CLI anymore)");
    println!();
    println!("Typical loop:");
    println!("  cargo xtask faf-ml build-web        # once (or after UI changes)");
    println!("  cargo xtask faf-ml backend          # then browse http://localhost:3100");
}

pub fn print_top_help() {
    println!("xtask — development tasks for the Octopus workspace");
    println!();
    println!("Usage: cargo xtask <app> <command> [args]");
    println!("       cargo xtask <global-command>");
    println!();
    println!("Apps:");
    println!("  fafcn      FAF construction simulator (Dioxus frontend + Axum backend)");
    println!("  faf-sim    FAF eco/build simulator");
    println!("  faf-ml     FAF unit-detection ML platform (Dioxus frontend + Axum backend)");
    println!("  qqbot      QQ bot service manager");
    println!();
    println!("Global commands:");
    println!("  test       Run cargo test --workspace");
    println!();
    println!("Examples:");
    println!("  cargo xtask fafcn backend");
    println!("  cargo xtask fafcn frontend");
    println!("  cargo xtask faf-sim");
    println!("  cargo xtask faf-sim --release");
    println!("  cargo xtask faf-sim web");
    println!("  cargo xtask faf-sim web build --release");
    println!("  cargo xtask faf-sim web serve --port 3000");
    println!("  cargo xtask faf-ml build-web");
    println!("  cargo xtask faf-ml backend");
    println!("  cargo xtask faf-ml frontend");
    println!("  cargo xtask qqbot build");
    println!("  cargo xtask qqbot start");
    println!("  cargo xtask qqbot status");
    println!("  cargo xtask qqbot logs core -n 50");
    println!("  cargo xtask qqbot deploy");
    println!("  cargo xtask qqbot remote-status");
    println!("  cargo xtask test");
}
