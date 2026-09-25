use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::approval_runtime::ApprovalRuntime;
use crate::auth::OAuthManager;
use crate::background::BackgroundTaskManager;
use crate::cli::UiMode;
use crate::config::Config;
use crate::llm::LLM;
use crate::notifications::manager::NotificationManager;
use crate::session::Session;
use crate::skills::Skill;
use crate::soul::approval::{Approval, ApprovalState};
use crate::soul::toolset::KimiToolset;
use crate::subagents::{LaborMarket, SubagentStore, SubagentType};
use crate::wire::RootWireHub;

#[derive(Debug, Clone)]
pub struct BuiltinSystemPromptArgs {
    pub kimi_now: String,
    pub kimi_work_dir: PathBuf,
    pub kimi_work_dir_ls: String,
    pub kimi_agents_md: String,
    pub kimi_skills: String,
    pub kimi_additional_dirs_info: String,
    pub kimi_os: String,
    pub kimi_shell: String,
}

#[derive(Debug, Clone)]
pub struct Dmail {
    pub checkpoint_id: usize,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("DenwaRenji error: {0}")]
pub struct DenwaRenjiError(pub String);

#[derive(Debug, Clone)]
pub struct DenwaRenji {
    pending_dmail: Option<Dmail>,
    n_checkpoints: usize,
}

impl DenwaRenji {
    pub fn new() -> Self {
        Self {
            pending_dmail: None,
            n_checkpoints: 0,
        }
    }

    pub fn send_dmail(&mut self, dmail: Dmail) -> Result<(), DenwaRenjiError> {
        if self.pending_dmail.is_some() {
            return Err(DenwaRenjiError(
                "Only one D-Mail can be sent at a time".to_string(),
            ));
        }
        if dmail.checkpoint_id >= self.n_checkpoints {
            return Err(DenwaRenjiError(format!(
                "There is no checkpoint with the given ID (max: {})",
                self.n_checkpoints.saturating_sub(1)
            )));
        }
        self.pending_dmail = Some(dmail);
        Ok(())
    }

    pub fn set_n_checkpoints(&mut self, n: usize) {
        self.n_checkpoints = n;
    }

    pub fn fetch_pending_dmail(&mut self) -> Option<Dmail> {
        self.pending_dmail.take()
    }
}

impl Default for DenwaRenji {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct Environment {
    pub os_kind: String,
    pub shell_name: String,
    pub shell_path: String,
}

impl Environment {
    pub async fn detect() -> Self {
        Self::detect_blocking()
    }
}

fn detect_shell() -> (String, String) {
    let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let shell_name = std::path::Path::new(&shell_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sh")
        .to_string();
    (shell_name, shell_path)
}

#[derive(Debug, Clone)]
pub struct AppRuntime {
    pub config: Config,
    pub oauth: OAuthManager,
    pub llm: Option<LLM>,
    pub session: Session,
    pub builtin_args: BuiltinSystemPromptArgs,
    pub denwa_renji: DenwaRenji,
    pub approval: Approval,
    pub labor_market: LaborMarket,
    pub environment: Environment,
    pub notifications: NotificationManager,
    pub background_tasks: BackgroundTaskManager,
    pub skills: HashMap<String, Skill>,
    pub additional_dirs: Vec<PathBuf>,
    pub skills_dirs: Vec<PathBuf>,
    pub subagent_store: Option<SubagentStore>,
    pub approval_runtime: Option<ApprovalRuntime>,
    pub root_wire_hub: Option<RootWireHub>,
    pub subagent_id: Option<String>,
    pub subagent_type: Option<SubagentType>,
    pub role: String,
    pub ui_mode: UiMode,
    pub resumed: bool,
}

impl AppRuntime {
    pub fn new(
        config: Config,
        session: Session,
        llm: Option<LLM>,
        approval: Approval,
        builtin_args: BuiltinSystemPromptArgs,
    ) -> Self {
        let subagent_store = Some(SubagentStore::new(&session));
        let notification_root = crate::share::get_share_dir()
            .join("notifications")
            .join(&session.id);
        let notifications =
            NotificationManager::new(notification_root, config.notifications.clone());
        Self {
            config,
            oauth: OAuthManager::new(),
            llm,
            session,
            builtin_args,
            denwa_renji: DenwaRenji::new(),
            approval,
            labor_market: LaborMarket::new(),
            environment: Environment::detect_blocking(),
            notifications,
            background_tasks: BackgroundTaskManager::new(),
            skills: HashMap::new(),
            additional_dirs: Vec::new(),
            skills_dirs: Vec::new(),
            subagent_store,
            approval_runtime: Some(ApprovalRuntime::new()),
            root_wire_hub: Some(RootWireHub::new()),
            subagent_id: None,
            subagent_type: None,
            role: "root".to_string(),
            ui_mode: UiMode::Shell,
            resumed: false,
        }
    }

    /// Create a child runtime for a subagent, sharing the parent's
    /// LaborMarket, skills, notifications, background tasks, and denwa renji.
    pub fn copy_for_subagent(
        &self,
        session: Session,
        llm: Option<LLM>,
        approval: Approval,
        builtin_args: BuiltinSystemPromptArgs,
        subagent_type: Option<SubagentType>,
    ) -> Self {
        let session_id = session.id.clone();
        Self {
            config: self.config.clone(),
            oauth: OAuthManager::new(),
            llm,
            session,
            builtin_args,
            denwa_renji: self.denwa_renji.clone(),
            approval,
            labor_market: self.labor_market.clone(),
            environment: self.environment.clone(),
            notifications: self.notifications.clone(),
            background_tasks: self.background_tasks.clone(),
            skills: self.skills.clone(),
            additional_dirs: self.additional_dirs.clone(),
            skills_dirs: self.skills_dirs.clone(),
            subagent_store: self.subagent_store.clone(),
            approval_runtime: self.approval_runtime.clone(),
            root_wire_hub: Some(RootWireHub::new()),
            subagent_id: Some(session_id),
            subagent_type,
            role: "subagent".to_string(),
            ui_mode: self.ui_mode,
            resumed: false,
        }
    }
}

impl Environment {
    pub fn detect_blocking() -> Self {
        let (shell_name, shell_path) = detect_shell();
        Self {
            os_kind: std::env::consts::OS.to_string(),
            shell_name,
            shell_path,
        }
    }
}

pub struct Agent {
    pub name: String,
    pub system_prompt: String,
    pub toolset: KimiToolset,
    pub runtime: AppRuntime,
}

impl Agent {
    pub fn new_basic(
        name: String,
        system_prompt: String,
        config: Config,
        session: Session,
        llm: Option<LLM>,
        approval: ApprovalState,
        mcp_configs: Vec<crate::mcp::McpConfig>,
    ) -> Self {
        let builtin_args = BuiltinSystemPromptArgs {
            kimi_now: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            kimi_work_dir: session.work_dir.clone(),
            kimi_work_dir_ls: String::new(),
            kimi_agents_md: String::new(),
            kimi_skills: String::new(),
            kimi_additional_dirs_info: String::new(),
            kimi_os: std::env::consts::OS.to_string(),
            kimi_shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        };
        let runtime = AppRuntime::new(
            config,
            session,
            llm,
            Approval::with_state(approval),
            builtin_args,
        );
        let mut toolset = KimiToolset::new();

        // Load WASM plugin tools even for basic agents (no agent file specified).
        if let Some(ref plugins_dir) = crate::plugin::default_plugins_dir()
            && plugins_dir.is_dir()
        {
            for plugin_tool in crate::plugin::discover_plugins(plugins_dir) {
                let plugin_name = plugin_tool.name().to_string();
                if toolset.find(&plugin_name).is_some() {
                    tracing::warn!(
                        "Plugin tool '{}' conflicts with an existing tool, skipping",
                        plugin_name
                    );
                    continue;
                }
                toolset.register(plugin_tool);
            }
        }

        // Defer MCP tool loading for basic agents too.
        if !mcp_configs.is_empty() {
            let mcp_context = crate::soul::toolset::McpLoadContext {
                tool_call_timeout_ms: 60_000,
            };
            toolset.defer_mcp_tool_loading(mcp_configs, mcp_context);
        }

        Self {
            name,
            system_prompt,
            toolset,
            runtime,
        }
    }
}

/// Load the system prompt from a file and render it with Jinja2-style
/// templating using `${...}` delimiters.
fn load_system_prompt(
    path: &Path,
    spec_args: &HashMap<String, String>,
    builtin_args: &BuiltinSystemPromptArgs,
) -> crate::exception::Result<String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        crate::exception::OctopusError::Other(format!(
            "Failed to read system prompt {}: {}",
            path.display(),
            e
        ))
    })?;

    let mut env = minijinja::Environment::new();
    env.set_syntax(
        minijinja::syntax::SyntaxConfig::builder()
            .variable_delimiters("${", "}")
            .build()
            .unwrap(),
    );

    let template = env.template_from_str(&text).map_err(|e| {
        crate::exception::OctopusError::Other(format!(
            "Invalid system prompt template in {}: {}",
            path.display(),
            e
        ))
    })?;

    let mut ctx_map = serde_json::Map::new();
    ctx_map.insert(
        "KIMI_NOW".to_string(),
        serde_json::Value::String(builtin_args.kimi_now.clone()),
    );
    ctx_map.insert(
        "KIMI_WORK_DIR".to_string(),
        serde_json::Value::String(builtin_args.kimi_work_dir.to_string_lossy().to_string()),
    );
    ctx_map.insert(
        "KIMI_WORK_DIR_LS".to_string(),
        serde_json::Value::String(builtin_args.kimi_work_dir_ls.clone()),
    );
    ctx_map.insert(
        "KIMI_AGENTS_MD".to_string(),
        serde_json::Value::String(builtin_args.kimi_agents_md.clone()),
    );
    ctx_map.insert(
        "KIMI_SKILLS".to_string(),
        serde_json::Value::String(builtin_args.kimi_skills.clone()),
    );
    ctx_map.insert(
        "KIMI_ADDITIONAL_DIRS_INFO".to_string(),
        serde_json::Value::String(builtin_args.kimi_additional_dirs_info.clone()),
    );
    ctx_map.insert(
        "KIMI_OS".to_string(),
        serde_json::Value::String(builtin_args.kimi_os.clone()),
    );
    ctx_map.insert(
        "KIMI_SHELL".to_string(),
        serde_json::Value::String(builtin_args.kimi_shell.clone()),
    );
    for (k, v) in spec_args {
        ctx_map.insert(k.clone(), serde_json::Value::String(v.clone()));
    }

    let rendered = template.render(ctx_map).map_err(|e| {
        crate::exception::OctopusError::Other(format!(
            "Failed to render system prompt template in {}: {}",
            path.display(),
            e
        ))
    })?;

    Ok(rendered.trim().to_string())
}

/// Build a built-in tool implementation.
///
/// Returns `Option` because `TaskList` and `ReadMediaFile` are referenced in
/// agent YAML specs but not yet ported to Rust. Once those tools are either
/// implemented or removed from the YAML specs, this function should return
/// `Box<dyn CallableTool>` directly and the `Option` wrapper can be removed.
fn build_tool(
    tool: crate::tools::tool_name::BuiltinTool,
    runtime: &AppRuntime,
) -> Option<Box<dyn llm_provider::tooling::CallableTool>> {
    use crate::tools::tool_name::BuiltinTool;
    use llm_provider::tooling::CallableTool2Adapter;
    match tool {
        // Shell & background
        BuiltinTool::Shell => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::shell::ShellTool::new(runtime.background_tasks.clone()),
        ))),
        BuiltinTool::TaskOutput => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::background::TaskOutputTool::new(runtime.background_tasks.clone()),
        ))),
        BuiltinTool::TaskStop => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::background::TaskStopTool::new(runtime.background_tasks.clone()),
        ))),
        BuiltinTool::TaskList => {
            // Not yet ported; skip silently.
            None
        }

        // File
        BuiltinTool::ReadFile => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::file::ReadFileTool::new(),
        ))),
        BuiltinTool::ReadMediaFile => {
            // Not yet ported.
            None
        }
        BuiltinTool::WriteFile => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::file::WriteFileTool::new(),
        ))),
        BuiltinTool::StrReplaceFile => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::file::StrReplaceFileTool::new(),
        ))),
        BuiltinTool::Glob => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::file::GlobTool::new(),
        ))),
        BuiltinTool::Grep => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::file::GrepTool::new(),
        ))),

        // Web
        BuiltinTool::SearchWeb => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::web::SearchWebTool::new(),
        ))),
        BuiltinTool::FetchURL => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::web::FetchURLTool::new(),
        ))),

        // Ask user / todo / think / plan
        BuiltinTool::AskUserQuestion => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::ask_user::AskUserTool::new(),
        ))),
        BuiltinTool::SetTodoList => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::todo::SetTodoListTool::new(),
        ))),
        BuiltinTool::Think => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::think::ThinkTool::new(),
        ))),
        BuiltinTool::ExitPlanMode => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::plan::ExitPlanModeTool::new(),
        ))),
        BuiltinTool::EnterPlanMode => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::plan::EnterPlanModeTool::new(),
        ))),

        // Agent / D-Mail
        BuiltinTool::Agent => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::agent::AgentTool::new(runtime.clone()),
        ))),
        BuiltinTool::SendDMail => Some(Box::new(CallableTool2Adapter::new(
            crate::tools::dmail::SendDMailTool::new(std::sync::Arc::new(std::sync::Mutex::new(
                runtime.denwa_renji.clone(),
            ))),
        ))),
    }
}

/// Load an agent from a specification file, mirroring Python's
/// `kimi_cli.soul.agent.load_agent`.
///
/// 1. Parse the YAML agent spec (with inheritance).
/// 2. Render the system prompt through Jinja2 (`${...}` delimiters).
/// 3. Build the toolset from the spec's tool list.
/// 4. Register subagent types in the LaborMarket.
/// 5. Load plugin tools.
/// 6. Defer MCP tool loading if configs are provided.
/// 7. Return a fully populated `Agent`.
pub async fn load_agent(
    agent_file: &Path,
    runtime: AppRuntime,
    mcp_configs: Vec<crate::mcp::McpConfig>,
) -> crate::exception::Result<Agent> {
    let spec = crate::agents::load_agent_spec(agent_file)?;

    let system_prompt = load_system_prompt(
        &spec.system_prompt_path,
        &spec.system_prompt_args,
        &runtime.builtin_args,
    )?;

    let mut toolset = KimiToolset::new();

    // Determine effective tool list.
    let tools = spec.allowed_tools.unwrap_or(spec.tools);
    let excluded: std::collections::HashSet<crate::tools::tool_name::BuiltinTool> =
        spec.exclude_tools.into_iter().collect();

    for tool_name in tools {
        if excluded.contains(&tool_name) {
            continue;
        }
        if let Some(tool) = build_tool(tool_name, &runtime) {
            toolset.register(tool);
        }
    }

    // Register subagent types in the LaborMarket so the Agent tool can look them up.
    for (subagent_name, subagent_spec) in spec.subagents {
        tracing::info!(
            "Registering subagent type: {} -> {}",
            subagent_name,
            subagent_spec.path.display()
        );

        let subagent_file = &subagent_spec.path;
        let builtin_spec = match crate::agents::load_agent_spec(subagent_file) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Failed to load subagent spec for '{}': {}, skipping",
                    subagent_name,
                    e
                );
                continue;
            }
        };

        let tool_policy = if let Some(ref allowed) = builtin_spec.allowed_tools {
            crate::subagents::ToolPolicy::AllowList {
                tools: allowed.iter().copied().map(Into::into).collect(),
            }
        } else {
            crate::subagents::ToolPolicy::Inherit
        };

        runtime
            .labor_market
            .add_builtin_type(crate::subagents::AgentTypeDefinition {
                name: crate::subagents::SubagentType::from(subagent_name.as_str()),
                description: Some(subagent_spec.description),
                agent_file: subagent_spec.path,
                when_to_use: if builtin_spec.when_to_use.is_empty() {
                    None
                } else {
                    Some(builtin_spec.when_to_use)
                },
                default_model: builtin_spec.model,
                tool_policy,
            });
    }

    // Load WASM plugin tools from ~/.kimi/plugins/
    if let Some(ref plugins_dir) = crate::plugin::default_plugins_dir()
        && plugins_dir.is_dir()
    {
        for plugin_tool in crate::plugin::discover_plugins(plugins_dir) {
            let plugin_name = plugin_tool.name().to_string();
            if toolset.find(&plugin_name).is_some() {
                tracing::warn!(
                    "Plugin tool '{}' conflicts with an existing tool, skipping",
                    plugin_name
                );
                continue;
            }
            toolset.register(plugin_tool);
        }
    }

    // Defer MCP tool loading.
    if !mcp_configs.is_empty() {
        let mcp_context = crate::soul::toolset::McpLoadContext {
            tool_call_timeout_ms: 60_000,
        };
        toolset.defer_mcp_tool_loading(mcp_configs, mcp_context);
    }

    Ok(Agent {
        name: spec.name,
        system_prompt,
        toolset,
        runtime,
    })
}
