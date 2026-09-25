use std::path::PathBuf;
use std::process;

use octopus_cli::app::{OctopusCLI, enable_logging};
use octopus_cli::cli::{AgentChoice, Cli, Commands, InputFormat, OutputFormat, UiMode};
use octopus_cli::constant::get_version;
use octopus_cli::session::Session;

fn main() {
    let cli = <Cli as clap::Parser>::parse();

    // Version is handled by clap's version action
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("kimi, version {}", get_version());
        process::exit(0);
    }

    // Handle subcommands
    if let Some(command) = cli.command {
        match command {
            Commands::Login { json } => {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                let manager = octopus_cli::auth::OAuthManager::new();
                match runtime.block_on(manager.login("kimi-code")) {
                    Ok(()) => {
                        if json {
                            println!(
                                "{{\"event\":\"success\",\"message\":\"Logged in successfully\"}}"
                            );
                        }
                    }
                    Err(e) => {
                        if json {
                            println!("{{\"event\":\"error\",\"message\":\"{}\"}}", e);
                        } else {
                            eprintln!("Login failed: {}", e);
                        }
                        std::process::exit(1);
                    }
                }
                return;
            }
            Commands::Logout { json } => {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                let manager = octopus_cli::auth::OAuthManager::new();
                match runtime.block_on(manager.logout("kimi-code")) {
                    Ok(()) => {
                        if json {
                            println!(
                                "{{\"event\":\"success\",\"message\":\"Logged out successfully\"}}"
                            );
                        }
                    }
                    Err(e) => {
                        if json {
                            println!("{{\"event\":\"error\",\"message\":\"{}\"}}", e);
                        } else {
                            eprintln!("Logout failed: {}", e);
                        }
                        std::process::exit(1);
                    }
                }
                return;
            }
            Commands::Term => {
                println!("Term (Toad TUI) is not yet implemented in octopus-cli.");
                return;
            }
            Commands::Acp => {
                println!("ACP server is not yet implemented in octopus-cli.");
                return;
            }
            Commands::BackgroundTaskWorker { .. } => {
                println!("Background task worker is not yet implemented in octopus-cli.");
                return;
            }
            Commands::WebWorker { .. } => {
                println!("Web worker is not yet implemented in octopus-cli.");
                return;
            }
            Commands::Export {
                session_id,
                output,
                yes,
            } => {
                let runtime =
                    tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
                let work_dir = cli
                    .work_dir
                    .map(|p| p.canonicalize().unwrap_or(p))
                    .unwrap_or_else(|| {
                        std::env::current_dir().expect("Failed to get current directory")
                    });
                runtime.block_on(async {
                    if let Err(e) =
                        octopus_cli::cli::export::run_export(&work_dir, session_id, output, yes)
                            .await
                    {
                        eprintln!("Export failed: {}", e);
                        std::process::exit(1);
                    }
                });
                return;
            }
            Commands::Info { json } => {
                octopus_cli::cli::info::run_info(json);
                return;
            }
            Commands::Plugin { command } => {
                match command {
                    Some(octopus_cli::cli::PluginCommands::Install { target }) => {
                        println!("Plugin install not yet implemented: {}", target);
                    }
                    Some(octopus_cli::cli::PluginCommands::List) => {
                        println!("Plugin list not yet implemented");
                    }
                    Some(octopus_cli::cli::PluginCommands::Remove { name }) => {
                        println!("Plugin remove not yet implemented: {}", name);
                    }
                    Some(octopus_cli::cli::PluginCommands::Info { name }) => {
                        println!("Plugin info not yet implemented: {}", name);
                    }
                    None => {
                        println!("Plugin management commands: install, list, remove, info");
                    }
                }
                return;
            }
            Commands::Toad => {
                println!("Toad TUI is not yet implemented in octopus-cli.");
                return;
            }
            Commands::Web => {
                println!("Web UI server is not yet implemented in octopus-cli.");
                return;
            }
            Commands::Vis => {
                println!("Visualizer server is not yet implemented in octopus-cli.");
                return;
            }
        }
    }

    // Main CLI flow
    let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
    runtime.block_on(async_main(cli));
}

fn print_fatal_error(message: &str) {
    eprintln!("{}", message);
}

async fn async_main(cli: Cli) {
    enable_logging(cli.debug, false);

    let work_dir = cli
        .work_dir
        .clone()
        .map(|p| p.canonicalize().unwrap_or(p))
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    // Conflict checks
    let agent_conflict = cli.agent.is_some() && cli.agent_file.is_some();
    let continue_conflict =
        cli.continue_ && (cli.session.is_some() || cli.session.as_deref() == Some(""));

    match (agent_conflict, continue_conflict) {
        (false, false) => {}
        (true, _) => {
            print_fatal_error("Cannot combine --agent and --agent-file");
            process::exit(1);
        }
        (_, true) => {
            print_fatal_error("Cannot combine --continue and --session");
            process::exit(1);
        }
    }

    // Determine agent file
    let agent_file = if let Some(ref file) = cli.agent_file {
        Some(file.clone())
    } else {
        match cli.agent {
            Some(AgentChoice::Default) => Some(PathBuf::from("agents/default/agent.yaml")),
            Some(AgentChoice::Okabe) => Some(PathBuf::from("agents/okabe/agent.yaml")),
            None => None,
        }
    };
    // Parse MCP configs
    let mut mcp_configs: Vec<serde_json::Value> = Vec::new();
    for file in &cli.mcp_config_file {
        match std::fs::read_to_string(file) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(val) => mcp_configs.push(val),
                Err(e) => {
                    eprintln!("Invalid JSON in MCP config file {}: {}", file.display(), e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("Failed to read MCP config file {}: {}", file.display(), e);
                std::process::exit(1);
            }
        }
    }
    for config_str in &cli.mcp_config {
        match serde_json::from_str(config_str) {
            Ok(val) => mcp_configs.push(val),
            Err(e) => {
                eprintln!("Invalid JSON in MCP config: {}: {}", config_str, e);
                std::process::exit(1);
            }
        }
    }
    let mcp_configs: Vec<octopus_cli::mcp::McpConfig> = mcp_configs
        .into_iter()
        .map(|val| match serde_json::from_value(val) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("Invalid MCP config structure: {}", e);
                std::process::exit(1);
            }
        })
        .collect();

    // Determine UI mode
    let mut ui_mode = cli.ui_mode();

    // Handle quiet mode
    let (output_format_val, final_message_only) = if cli.quiet {
        if matches!(ui_mode, UiMode::Acp | UiMode::Wire) {
            eprintln!("Quiet mode cannot be combined with ACP or Wire UI");
            std::process::exit(1);
        }
        if cli.output_format.is_some() && !matches!(cli.output_format, Some(OutputFormat::Text)) {
            eprintln!("Quiet mode implies --output-format text");
            std::process::exit(1);
        }
        ui_mode = UiMode::Print;
        (Some(OutputFormat::Text), true)
    } else {
        (cli.output_format.clone(), cli.final_message_only)
    };

    // Resolve config source
    let config_source = cli.config_source();

    // Determine session
    let mut session_id = cli.session.clone();
    let picker_mode = session_id.as_deref() == Some("");
    if session_id.as_deref() == Some("") {
        session_id = None;
    }

    let continue_ = cli.continue_;

    // Prompt validation
    let prompt = cli
        .prompt
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    if let Some(ref p) = prompt
        && p.is_empty()
    {
        eprintln!("Prompt cannot be empty");
        std::process::exit(1);
    }

    // Input/output format validation
    match (
        ui_mode,
        picker_mode,
        cli.input_format.is_some(),
        cli.output_format.is_some(),
        cli.final_message_only,
    ) {
        (UiMode::Shell, false, false, false, false) => {}
        (UiMode::Print, false, _, _, _) => {}
        (UiMode::Acp | UiMode::Wire, false, false, false, false) => {}
        (_, _, true, _, _) => {
            eprintln!("Input format is only supported for print UI");
            std::process::exit(1);
        }
        (_, _, _, true, _) => {
            eprintln!("Output format is only supported for print UI");
            std::process::exit(1);
        }
        (_, _, _, _, true) => {
            eprintln!("Final-message-only output is only supported for print UI");
            std::process::exit(1);
        }
        (_, true, _, _, _) => {
            eprintln!("--session without a session ID is only supported for shell UI");
            std::process::exit(1);
        }
    }

    // Handle picker mode
    if picker_mode {
        let sessions = Session::list(&work_dir).await;
        if sessions.is_empty() {
            println!("No sessions found for the working directory.");
            std::process::exit(0);
        }
        match octopus_cli::ui::picker::pick_session_interactive(sessions) {
            Ok(Some(id)) => {
                session_id = Some(id);
            }
            Ok(None) => {
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Picker error: {}", e);
                std::process::exit(1);
            }
        }
    }

    let last_session_id = session_id.clone();

    // Determine how to obtain the session.
    enum SessionSource {
        Resume(String),
        Continue,
        New,
    }

    let session_source = if let Some(ref sid) = last_session_id {
        SessionSource::Resume(sid.clone())
    } else if continue_ {
        SessionSource::Continue
    } else {
        SessionSource::New
    };

    // Main flow (kept as a block so reload support can re-wrap it in a loop)
    let exit_code;

    {
        let session = match session_source {
            SessionSource::Resume(ref sid) => match Session::find(&work_dir, sid).await {
                Some(s) => {
                    tracing::info!("Resuming session: {}", sid);
                    s
                }
                None => {
                    tracing::info!("Session {} not found, creating new session", sid);
                    match Session::create(&work_dir, Some(sid.clone())).await {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Failed to create session: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            },
            SessionSource::Continue => match Session::continue_(&work_dir).await {
                Some(s) => {
                    tracing::info!("Continuing previous session: {}", s.id);
                    s
                }
                None => {
                    eprintln!("No previous session found for the working directory");
                    std::process::exit(1);
                }
            },
            SessionSource::New => match Session::create(&work_dir, None).await {
                Ok(s) => {
                    tracing::info!("Created new session: {}", s.id);
                    s
                }
                Err(e) => {
                    eprintln!("Failed to create session: {}", e);
                    std::process::exit(1);
                }
            },
        };

        let resumed = matches!(
            session_source,
            SessionSource::Resume(_) | SessionSource::Continue
        );

        let approval_mode = if cli.yolo {
            octopus_cli::soul::approval::ApprovalMode::Yolo
        } else if cli.afk {
            octopus_cli::soul::approval::ApprovalMode::Afk
        } else {
            octopus_cli::soul::approval::ApprovalMode::Ask
        };

        let mut instance = match OctopusCLI::create(octopus_cli::app::CreateOptions {
            session,
            config_source: config_source.clone(),
            model_name: cli.model.clone(),
            approval_mode,
            resumed,
            ui_mode,
            max_steps_per_turn: cli.max_steps_per_turn,
            max_retries_per_step: cli.max_retries_per_step,
            max_ralph_iterations: cli.max_ralph_iterations,
            agent_file: agent_file.clone(),
            mcp_configs: mcp_configs.clone(),
        })
        .await
        {
            Ok(i) => i,
            Err(e) => {
                eprintln!("Failed to initialize Kimi CLI: {}", e);
                std::process::exit(1);
            }
        };

        let result = match ui_mode {
            UiMode::Print => {
                instance
                    .run_print(
                        cli.input_format.clone().unwrap_or(InputFormat::Text),
                        output_format_val.clone().unwrap_or(OutputFormat::Text),
                        prompt.clone(),
                        final_message_only,
                    )
                    .await
            }
            UiMode::Acp => instance.run_acp().await.map(|_| 0),
            UiMode::Wire => instance.run_wire_stdio().await.map(|_| 0),
            UiMode::Shell => instance
                .run_shell(prompt.clone(), None)
                .await
                .map(|ok| if ok { 0 } else { 1 }),
        };

        match result {
            Ok(code) => {
                exit_code = code;
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }

    std::process::exit(exit_code);
}
