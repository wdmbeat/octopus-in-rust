use std::collections::HashMap;
use std::path::PathBuf;

use tracing;

use crate::cli::{ConfigSource, UiMode};
use crate::config::{Config, LLMModel, LLMProvider, load_config, load_config_from_string};
use crate::exception::Result;
use crate::llm::{LLM, augment_provider_with_env_vars, create_llm};
use crate::session::Session;

use crate::soul::{ApprovalState, KimiSoul};

pub fn enable_logging(debug: bool, redirect_stderr: bool) {
    let filter = if debug {
        tracing::level_filters::LevelFilter::DEBUG
    } else {
        tracing::level_filters::LevelFilter::INFO
    };

    tracing_subscriber::fmt()
        .with_max_level(filter)
        .with_target(true)
        .init();

    let _ = redirect_stderr;
}

pub struct OctopusCLI {
    pub soul: Option<KimiSoul>,
    pub runtime: AppRuntime,
    pub env_overrides: HashMap<String, String>,
}

/// Parameters for [`OctopusCLI::create`].
pub struct CreateOptions {
    pub session: Session,
    pub config_source: Option<ConfigSource>,
    pub model_name: Option<String>,
    pub approval_mode: crate::soul::approval::ApprovalMode,
    pub resumed: bool,
    pub ui_mode: UiMode,
    pub max_steps_per_turn: Option<usize>,
    pub max_retries_per_step: Option<usize>,
    pub max_ralph_iterations: Option<i32>,
    pub agent_file: Option<PathBuf>,
    pub mcp_configs: Vec<crate::mcp::McpConfig>,
}

#[derive(Debug, Clone)]
pub struct AppRuntime {
    pub config: Config,
    pub session: Session,
    pub llm: Option<LLM>,
    pub approval: ApprovalRuntime,
    pub ui_mode: UiMode,
    pub resumed: bool,
}

#[derive(Debug, Clone)]
pub struct ApprovalRuntime {
    mode: crate::soul::approval::ApprovalMode,
}

impl ApprovalRuntime {
    pub fn is_yolo(&self) -> bool {
        self.mode.is_yolo()
    }

    pub fn is_afk(&self) -> bool {
        self.mode.is_afk()
    }
}

impl OctopusCLI {
    pub async fn create(options: CreateOptions) -> Result<Self> {
        let CreateOptions {
            session,
            config_source,
            model_name,
            approval_mode,
            resumed,
            ui_mode,
            max_steps_per_turn,
            max_retries_per_step,
            max_ralph_iterations,
            agent_file,
            mcp_configs,
        } = options;
        // 1. Load configuration and apply CLI overrides.
        // 1.1 Load config from the provided source (inline, file, or default location).
        let mut config = match config_source {
            Some(ConfigSource::Inline(s)) => load_config_from_string(&s)?,
            Some(ConfigSource::File(p)) => load_config(Some(&p))?,
            None => load_config(None)?,
        };

        // 1.2 Apply CLI overrides for loop-control settings.
        if let Some(max_steps) = max_steps_per_turn {
            config.loop_control.max_steps_per_turn = max_steps;
        }
        if let Some(max_retries) = max_retries_per_step {
            config.loop_control.max_retries_per_step = max_retries;
        }
        if let Some(max_ralph) = max_ralph_iterations {
            config.loop_control.max_ralph_iterations = max_ralph;
        }

        // 2. Resolve model and provider.
        // 2.1 Look up explicit model (from --model) and default model (from config).
        let explicit = model_name.as_ref().and_then(|n| config.models.get(n));
        let default = if config.default_model.is_empty() {
            None
        } else {
            config.models.get(&config.default_model)
        };

        // 2.2 Pre-compute existence booleans for explicit/default lookups.
        let name_given = model_name.is_some();
        let name_exists = explicit.is_some();
        let default_given = !config.default_model.is_empty();
        let default_exists = default.is_some();

        // 2.3 Match on the tuple to pick model/provider with clear priority.
        let (mut model, mut provider) =
            match (name_given, name_exists, default_given, default_exists) {
                // Explicit model requested and found in config.
                (true, true, _, _) => {
                    let m = explicit.unwrap().clone();
                    let p = config
                        .providers
                        .get(&m.provider)
                        .cloned()
                        .unwrap_or_else(|| LLMProvider {
                            provider_type: crate::config::ProviderType::Kimi,
                            base_url: String::new(),
                            api_key: None,
                            env: None,
                            custom_headers: None,
                            reasoning_key: None,
                            oauth: None,
                        });
                    (m, p)
                }
                // No explicit model; default configured and found in config.
                (false, _, true, true) => {
                    let m = default.unwrap().clone();
                    let p = config
                        .providers
                        .get(&m.provider)
                        .cloned()
                        .unwrap_or_else(|| LLMProvider {
                            provider_type: crate::config::ProviderType::Kimi,
                            base_url: String::new(),
                            api_key: None,
                            env: None,
                            custom_headers: None,
                            reasoning_key: None,
                            oauth: None,
                        });
                    (m, p)
                }
                // Everything else falls back to hard-coded defaults:
                //   - explicit name given but not found
                //   - no explicit name and no default configured
                //   - no explicit name and default configured but not found
                _ => {
                    let m = LLMModel {
                        provider: String::new(),
                        model: String::new(),
                        max_context_size: 100_000,
                        capabilities: None,
                        display_name: None,
                    };
                    let p = LLMProvider {
                        provider_type: crate::config::ProviderType::Kimi,
                        base_url: String::new(),
                        api_key: None,
                        env: None,
                        custom_headers: None,
                        reasoning_key: None,
                        oauth: None,
                    };
                    (m, p)
                }
            };

        // 2.4 Apply environment variable overrides to provider and model.
        let env_overrides = augment_provider_with_env_vars(&mut provider, &mut model);

        // 3. Resolve derived settings.
        // 3.2 Instantiate the LLM client only when provider and model are configured.
        let llm = if !provider.base_url.is_empty() && !model.model.is_empty() {
            create_llm(&provider, &model)
        } else {
            None
        };

        // 4. Build approval state from session state merged with CLI flags.
        // CLI > config default > persisted session state.
        let mode = if approval_mode != crate::soul::approval::ApprovalMode::Ask {
            approval_mode
        } else if config.default_yolo {
            crate::soul::approval::ApprovalMode::Yolo
        } else {
            session.state.approval.mode
        };
        let approval_state = ApprovalState {
            mode,
            auto_approve_actions: session.state.approval.auto_approve_actions.clone(),
        };

        // 5. Construct agent and soul.
        // 5.1 Build approval wrapper and builtin prompt arguments.
        let approval_wrapper = crate::soul::approval::Approval::with_state(approval_state.clone());
        let builtin_args = crate::soul::agent::BuiltinSystemPromptArgs {
            kimi_now: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            kimi_work_dir: session.work_dir.clone(),
            kimi_work_dir_ls: String::new(),
            kimi_agents_md: String::new(),
            kimi_skills: String::new(),
            kimi_additional_dirs_info: String::new(),
            kimi_os: std::env::consts::OS.to_string(),
            kimi_shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        };
        // 5.2 Build the AppRuntime needed for agent loading.
        let app_runtime = crate::soul::agent::AppRuntime::new(
            config.clone(),
            session.clone(),
            llm.clone(),
            approval_wrapper,
            builtin_args,
        );

        // 5.3 Load agent from file spec, or fall back to a default agent.
        let agent = if let Some(ref path) = agent_file {
            match crate::soul::agent::load_agent(path, app_runtime, mcp_configs.clone()).await {
                Ok(agent) => agent,
                Err(e) => {
                    tracing::warn!(
                        "Failed to load agent from {}: {}, using default",
                        path.display(),
                        e
                    );
                    crate::soul::agent::Agent::new_basic(
                        "default".to_string(),
                        "You are a helpful assistant.".to_string(),
                        config.clone(),
                        session.clone(),
                        llm.clone(),
                        approval_state.clone(),
                        mcp_configs.clone(),
                    )
                }
            }
        } else {
            crate::soul::agent::Agent::new_basic(
                "default".to_string(),
                "You are a helpful assistant.".to_string(),
                config.clone(),
                session.clone(),
                llm.clone(),
                approval_state.clone(),
                mcp_configs.clone(),
            )
        };

        // 5.4 Construct the KimiSoul (heart of the CLI).
        let soul = KimiSoul::new(
            config.clone(),
            session.clone(),
            llm.clone(),
            approval_state.clone(),
            agent,
            None,
        );

        // 6. Assemble the lightweight runtime exposed to the UI layer.
        // 6.1 Approval runtime (yolo/afk flags).
        let approval_runtime = ApprovalRuntime { mode };

        // 6.2 Full AppRuntime with all resolved components.
        let runtime = AppRuntime {
            config: config.clone(),
            session: session.clone(),
            llm: llm.clone(),
            approval: approval_runtime,
            ui_mode,
            resumed,
        };

        // 7. Initialize telemetry.
        // 7.1 Determine whether telemetry is disabled via config or env var.
        let telemetry_disabled =
            !config.telemetry || std::env::var("KIMI_DISABLE_TELEMETRY").is_ok();
        if telemetry_disabled {
            crate::telemetry::disable();
        } else {
            // 7.2 Set up the telemetry sink and start periodic flushing.
            let device_id = crate::telemetry::get_or_create_device_id();
            crate::telemetry::set_context(device_id.clone(), session.id.clone());
            let transport = crate::telemetry::transport::AsyncTransport::new(
                device_id,
                std::sync::Arc::new(|| None),
                crate::share::get_telemetry_dir(),
            );
            let ui_mode_str = format!("{:?}", ui_mode).to_lowercase();
            let sink = crate::telemetry::sink::EventSink::new(
                transport,
                String::new(),
                model.model.clone(),
                ui_mode_str.clone(),
            );
            sink.start_periodic_flush();
            crate::telemetry::attach_sink(sink);
            crate::telemetry::track_session_started_once(&ui_mode_str, resumed);
        }

        // 8. Return the fully initialized CLI handle.
        Ok(OctopusCLI {
            soul: Some(soul),
            runtime,
            env_overrides: env_overrides.into_iter().collect(),
        })
    }

    pub async fn run_shell(
        &mut self,
        command: Option<String>,
        prefill_text: Option<String>,
    ) -> Result<bool> {
        tracing::info!("Running shell UI");

        let soul = self.soul.take().ok_or_else(|| {
            crate::exception::OctopusError::Other("Soul already consumed".to_string())
        })?;

        let mut shell = crate::ui::shell::ShellUI::new(soul);
        let result = shell
            .run(command.or(prefill_text))
            .await
            .map_err(|e| crate::exception::OctopusError::Other(e.to_string()))?;

        Ok(result)
    }

    pub async fn run_print(
        &mut self,
        input_format: crate::cli::InputFormat,
        output_format: crate::cli::OutputFormat,
        command: Option<String>,
        // Currently unused: the print UI only ever emits the final message.
        _final_only: bool,
    ) -> Result<i32> {
        tracing::info!("Running print UI");

        let soul = self.soul.take().ok_or_else(|| {
            crate::exception::OctopusError::Other("Soul already consumed".to_string())
        })?;

        let mut print = crate::ui::print::PrintUI::new(soul, input_format, output_format);
        let result = print
            .run(command)
            .await
            .map_err(|e| crate::exception::OctopusError::Other(e.to_string()))?;

        Ok(result)
    }

    pub async fn run_acp(&mut self) -> Result<()> {
        tracing::info!("Running ACP server");
        println!("ACP server not yet implemented");
        Ok(())
    }

    pub async fn run_wire_stdio(&mut self) -> Result<()> {
        tracing::info!("Running Wire server over stdio");

        let soul = self.soul.take().ok_or_else(|| {
            crate::exception::OctopusError::Other("Soul already consumed".to_string())
        })?;

        let server = crate::wire_server::WireServer::new(soul);
        server.serve().await
    }

    pub async fn shutdown_background_tasks(&mut self) {
        if let Some(ref mut soul) = self.soul {
            soul.shutdown().await;
        }
    }

    pub async fn await_bg_tasks_shutdown(&mut self, _timeout: f64) {
        // Background task cleanup is handled synchronously by KimiSoul::shutdown()
    }
}
