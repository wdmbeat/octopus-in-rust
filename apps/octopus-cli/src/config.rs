use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::exception::{ConfigError, Result};
use crate::share::get_share_dir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Kimi,
    #[serde(alias = "openai_legacy")]
    OpenaiLegacy,
    #[serde(alias = "openai_responses")]
    OpenaiResponses,
    Anthropic,
    #[serde(alias = "google_genai")]
    Gemini,
    Vertexai,
    #[serde(rename = "_echo")]
    Echo,
    #[serde(rename = "_scripted_echo")]
    ScriptedEcho,
    #[serde(rename = "_chaos")]
    Chaos,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    ImageIn,
    VideoIn,
    Thinking,
    AlwaysThinking,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthRef {
    #[serde(default = "default_oauth_storage")]
    pub storage: String,
    pub key: String,
}

fn default_oauth_storage() -> String {
    "file".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMProvider {
    #[serde(rename = "type")]
    pub provider_type: ProviderType,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LLMModel {
    pub provider: String,
    pub model: String,
    pub max_context_size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<ModelCapability>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopControl {
    #[serde(alias = "max_steps_per_run")]
    pub max_steps_per_turn: usize,
    pub max_retries_per_step: usize,
    pub max_ralph_iterations: i32,
    pub reserved_context_size: usize,
    pub compaction_trigger_ratio: f64,
}

impl Default for LoopControl {
    fn default() -> Self {
        Self {
            max_steps_per_turn: 1000,
            max_retries_per_step: 3,
            max_ralph_iterations: 0,
            reserved_context_size: 50_000,
            compaction_trigger_ratio: 0.85,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundConfig {
    pub max_running_tasks: usize,
    pub read_max_bytes: usize,
    pub notification_tail_lines: usize,
    pub notification_tail_chars: usize,
    pub wait_poll_interval_ms: u64,
    pub worker_heartbeat_interval_ms: u64,
    pub worker_stale_after_ms: u64,
    pub kill_grace_period_ms: u64,
    pub keep_alive_on_exit: bool,
    pub agent_task_timeout_s: u64,
    pub print_wait_ceiling_s: u64,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        Self {
            max_running_tasks: 4,
            read_max_bytes: 30_000,
            notification_tail_lines: 20,
            notification_tail_chars: 3_000,
            wait_poll_interval_ms: 500,
            worker_heartbeat_interval_ms: 5_000,
            worker_stale_after_ms: 15_000,
            kill_grace_period_ms: 2_000,
            keep_alive_on_exit: false,
            agent_task_timeout_s: 900,
            print_wait_ceiling_s: 3600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    pub claim_stale_after_ms: u64,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            claim_stale_after_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoonshotSearchConfig {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoonshotFetchConfig {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Services {
    #[serde(skip_serializing_if = "Option::is_none", rename = "moonshot_search")]
    pub moonshot_search: Option<MoonshotSearchConfig>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "moonshot_fetch")]
    pub moonshot_fetch: Option<MoonshotFetchConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MCPClientConfig {
    pub tool_call_timeout_ms: u64,
}

impl Default for MCPClientConfig {
    fn default() -> Self {
        Self {
            tool_call_timeout_ms: 60_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MCPConfig {
    #[serde(default)]
    pub client: MCPClientConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDef {
    pub event: crate::hooks::HookEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// Compiled regex from `matcher`. Filled at load time; `#[serde(skip)]`
    /// because it is derived from `matcher` and not persisted.
    #[serde(skip)]
    pub compiled_matcher: Option<Regex>,
    pub command: String,
    #[serde(default = "default_hook_timeout")]
    pub timeout: u64,
}

impl HookDef {
    /// Compile `self.matcher` into `self.compiled_matcher`.
    /// Warnings are emitted for invalid regex patterns.
    pub fn compile_matcher(&mut self) {
        self.compiled_matcher = self.matcher.as_ref().and_then(|p| match Regex::new(p) {
            Ok(re) => Some(re),
            Err(e) => {
                tracing::warn!("Invalid regex in hook matcher '{}': {}", p, e);
                None
            }
        });
    }
}

fn default_hook_timeout() -> u64 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip, default)]
    pub is_from_default_location: bool,
    #[serde(skip, default)]
    pub source_file: Option<PathBuf>,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub default_thinking: bool,
    #[serde(default)]
    pub default_yolo: bool,
    #[serde(default)]
    pub skip_afk_prompt_injection: bool,
    #[serde(default)]
    pub default_plan_mode: bool,
    #[serde(default)]
    pub default_editor: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_show_thinking_stream")]
    pub show_thinking_stream: bool,
    #[serde(default)]
    pub models: HashMap<String, LLMModel>,
    #[serde(default)]
    pub providers: HashMap<String, LLMProvider>,
    #[serde(default)]
    pub loop_control: LoopControl,
    #[serde(default)]
    pub background: BackgroundConfig,
    #[serde(default)]
    pub notifications: NotificationConfig,
    #[serde(default)]
    pub services: Services,
    #[serde(default)]
    pub mcp: MCPConfig,
    #[serde(default)]
    pub hooks: Vec<HookDef>,
    #[serde(default = "default_merge_all_available_skills")]
    pub merge_all_available_skills: bool,
    #[serde(default)]
    pub extra_skill_dirs: Vec<String>,
    #[serde(default = "default_telemetry")]
    pub telemetry: bool,
    #[serde(default)]
    pub workspace_dirs: Vec<PathBuf>,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_show_thinking_stream() -> bool {
    true
}

fn default_merge_all_available_skills() -> bool {
    true
}

fn default_telemetry() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            is_from_default_location: false,
            source_file: None,
            default_model: String::new(),
            default_thinking: false,
            default_yolo: false,
            skip_afk_prompt_injection: false,
            default_plan_mode: false,
            default_editor: String::new(),
            theme: default_theme(),
            show_thinking_stream: default_show_thinking_stream(),
            models: HashMap::new(),
            providers: HashMap::new(),
            loop_control: LoopControl::default(),
            background: BackgroundConfig::default(),
            notifications: NotificationConfig::default(),
            services: Services::default(),
            mcp: MCPConfig::default(),
            hooks: Vec::new(),
            merge_all_available_skills: default_merge_all_available_skills(),
            extra_skill_dirs: Vec::new(),
            telemetry: default_telemetry(),
            workspace_dirs: Vec::new(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if !self.default_model.is_empty() && !self.models.contains_key(&self.default_model) {
            return Err(ConfigError::InvalidText(format!(
                "Default model '{}' not found in models",
                self.default_model
            ))
            .into());
        }
        for (name, model) in &self.models {
            if !self.providers.contains_key(&model.provider) {
                return Err(ConfigError::InvalidText(format!(
                    "Provider '{}' for model '{}' not found in providers",
                    model.provider, name
                ))
                .into());
            }
        }
        Ok(())
    }
}

pub fn get_config_file() -> PathBuf {
    get_share_dir().join("config.toml")
}

pub fn get_default_config() -> Config {
    Config::default()
}

fn migrate_json_config_to_toml() {
    let old = get_share_dir().join("config.json");
    let new = get_share_dir().join("config.toml");
    if !old.exists() || new.exists() {
        return;
    }
    let content = match std::fs::read_to_string(&old) {
        Ok(c) => c,
        Err(_) => return,
    };
    let config: Config = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = save_config(&config, Some(&new));
    let backup = old.with_extension("json.bak");
    let _ = std::fs::rename(&old, &backup);
}

pub fn load_config(config_file: Option<&Path>) -> Result<Config> {
    let default_config_file = get_config_file();
    let config_file = config_file
        .map(|p| p.to_path_buf())
        .unwrap_or(default_config_file.clone());
    let is_default_config_file = config_file == default_config_file;

    if is_default_config_file && !config_file.exists() {
        migrate_json_config_to_toml();
    }

    if !config_file.exists() {
        let mut config = get_default_config();
        config.is_from_default_location = is_default_config_file;
        config.source_file = Some(config_file.clone());
        save_config(&config, Some(&config_file))?;
        return Ok(config);
    }

    let config_text =
        std::fs::read_to_string(&config_file).map_err(|e| ConfigError::InvalidFile {
            path: config_file.display().to_string(),
            source: Box::new(e),
        })?;

    let mut config: Config = if config_file.extension().and_then(|s| s.to_str()) == Some("json") {
        serde_json::from_str(&config_text).map_err(|e| ConfigError::InvalidFile {
            path: config_file.display().to_string(),
            source: Box::new(e),
        })?
    } else {
        toml::from_str(&config_text).map_err(|e| ConfigError::InvalidFile {
            path: config_file.display().to_string(),
            source: Box::new(e),
        })?
    };

    config.is_from_default_location = is_default_config_file;
    config.source_file = Some(config_file);
    for hook in &mut config.hooks {
        hook.compile_matcher();
    }
    config.validate()?;
    Ok(config)
}

pub fn load_config_from_string(config_string: &str) -> Result<Config> {
    let config_string = config_string.trim();
    if config_string.is_empty() {
        return Err(ConfigError::EmptyConfig.into());
    }

    let mut config: Config = match serde_json::from_str(config_string) {
        Ok(c) => c,
        Err(json_err) => toml::from_str(config_string)
            .map_err(|toml_err| ConfigError::InvalidText(format!("{}; {}", json_err, toml_err)))?,
    };

    for hook in &mut config.hooks {
        hook.compile_matcher();
    }

    config.validate()?;
    Ok(config)
}

pub fn save_config(config: &Config, config_file: Option<&Path>) -> Result<()> {
    let default_config_file = get_config_file();
    let config_file = config_file.unwrap_or(&default_config_file);
    if let Some(parent) = config_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let config_data =
        serde_json::to_value(config).map_err(|e| ConfigError::InvalidText(e.to_string()))?;
    let text = if config_file.extension().and_then(|s| s.to_str()) == Some("json") {
        serde_json::to_string_pretty(&config_data)
            .map_err(|e| ConfigError::InvalidText(e.to_string()))?
    } else {
        toml::to_string_pretty(&config_data).map_err(|e| ConfigError::InvalidText(e.to_string()))?
    };
    std::fs::write(config_file, text)?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_def_deserializes_from_toml_with_event_string_only() {
        let toml_str = r#"
event = "PreToolUse"
matcher = "Shell"
command = "echo hello"
timeout = 10
"#;
        let def: HookDef = toml::from_str(toml_str).expect("should parse HookDef from TOML");
        assert_eq!(def.event.to_string(), "PreToolUse");
        assert_eq!(def.matcher, Some("Shell".to_string()));
        assert_eq!(def.command, "echo hello");
        assert_eq!(def.timeout, 10);
    }

    #[test]
    fn test_config_with_hooks_deserializes_from_toml() {
        let toml_str = r#"
default_model = ""

[[hooks]]
event = "PreToolUse"
matcher = "Shell"
command = "echo ok"
timeout = 10

[[hooks]]
event = "PostToolUse"
matcher = "WriteFile"
command = "prettier --write"
"#;
        let config: Config = toml::from_str(toml_str).expect("should parse Config from TOML");
        assert_eq!(config.hooks.len(), 2);

        let first = &config.hooks[0];
        assert_eq!(first.event.to_string(), "PreToolUse");
        assert_eq!(first.matcher, Some("Shell".to_string()));
        assert_eq!(first.command, "echo ok");
        assert_eq!(first.timeout, 10);

        let second = &config.hooks[1];
        assert_eq!(second.event.to_string(), "PostToolUse");
        assert_eq!(second.matcher, Some("WriteFile".to_string()));
        assert_eq!(second.command, "prettier --write");
        assert_eq!(second.timeout, 30); // default
    }

    #[test]
    fn test_hook_def_default_timeout() {
        let toml_str = r#"
event = "Stop"
command = "echo done"
"#;
        let def: HookDef = toml::from_str(toml_str).unwrap();
        assert_eq!(def.timeout, 30);
        assert_eq!(def.matcher, None);
    }

    #[test]
    fn test_config_without_hooks_is_empty_vec() {
        let toml_str = r#"
default_model = ""
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn test_hook_def_invalid_event_fails() {
        let toml_str = r#"
event = "UnknownEvent"
command = "echo bad"
"#;
        let result: std::result::Result<HookDef, _> = toml::from_str(toml_str);
        assert!(result.is_err(), "unknown event should fail to parse");
    }
}
