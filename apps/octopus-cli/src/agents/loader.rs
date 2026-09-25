use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::tools::tool_name::BuiltinTool;

const DEFAULT_AGENT_SPEC_VERSION: &str = "1";

/// Subagent specification within an agent file.
#[derive(Debug, Clone, Deserialize)]
pub struct SubagentSpec {
    pub path: PathBuf,
    pub description: String,
}

/// Raw agent specification as read from YAML.
/// `None` on a field means "inherit from parent".
#[derive(Debug, Clone, Deserialize)]
struct RawAgentSpec {
    #[serde(default)]
    extend: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    system_prompt_path: Option<PathBuf>,
    #[serde(default)]
    system_prompt_args: HashMap<String, String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    when_to_use: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    exclude_tools: Vec<String>,
    #[serde(default)]
    subagents: HashMap<String, SubagentSpec>,
}

/// Resolved agent specification with no inheritance markers.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub name: String,
    pub system_prompt_path: PathBuf,
    pub system_prompt_args: HashMap<String, String>,
    pub model: Option<String>,
    pub when_to_use: String,
    pub tools: Vec<BuiltinTool>,
    pub allowed_tools: Option<Vec<BuiltinTool>>,
    pub exclude_tools: Vec<BuiltinTool>,
    pub subagents: HashMap<String, SubagentSpec>,
}

/// Wrapper for the top-level YAML document.
#[derive(Debug, Deserialize)]
struct AgentYaml {
    version: Option<String>,
    agent: RawAgentSpec,
}

/// Return the path to the built-in default agent file.
pub fn default_agent_file() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/agents/default/agent.yaml"
    ))
}

/// Load and resolve an agent spec from a YAML file.
///
/// Handles the `extend` inheritance chain, resolves relative paths,
/// and validates required fields.
pub fn load_agent_spec(agent_file: &Path) -> crate::exception::Result<AgentSpec> {
    let raw = load_raw_agent_spec(agent_file)?;

    if raw.name.is_none() {
        return Err(crate::exception::OctopusError::Other(
            "Agent name is required".to_string(),
        ));
    }
    if raw.system_prompt_path.is_none() {
        return Err(crate::exception::OctopusError::Other(
            "System prompt path is required".to_string(),
        ));
    }
    if raw.tools.is_none() {
        return Err(crate::exception::OctopusError::Other(
            "Tools are required".to_string(),
        ));
    }

    let tools = parse_builtin_tools(raw.tools.unwrap_or_default());
    let allowed_tools = raw.allowed_tools.map(parse_builtin_tools);
    let exclude_tools = parse_builtin_tools(raw.exclude_tools);

    Ok(AgentSpec {
        name: raw.name.unwrap(),
        system_prompt_path: raw.system_prompt_path.unwrap(),
        system_prompt_args: raw.system_prompt_args,
        model: raw.model,
        when_to_use: raw.when_to_use.unwrap_or_default(),
        tools,
        allowed_tools,
        exclude_tools,
        subagents: raw.subagents,
    })
}

/// Recursively load a raw agent spec, resolving `extend` inheritance.
fn load_raw_agent_spec(agent_file: &Path) -> crate::exception::Result<RawAgentSpec> {
    if !agent_file.exists() {
        return Err(crate::exception::OctopusError::Other(format!(
            "Agent spec file not found: {}",
            agent_file.display()
        )));
    }
    if !agent_file.is_file() {
        return Err(crate::exception::OctopusError::Other(format!(
            "Agent spec path is not a file: {}",
            agent_file.display()
        )));
    }

    let text = std::fs::read_to_string(agent_file).map_err(|e| {
        crate::exception::OctopusError::Other(format!(
            "Failed to read agent spec {}: {}",
            agent_file.display(),
            e
        ))
    })?;

    let doc: AgentYaml = serde_yml::from_str(&text).map_err(|e| {
        crate::exception::OctopusError::Other(format!(
            "Invalid YAML in agent spec {}: {}",
            agent_file.display(),
            e
        ))
    })?;

    let version = doc.version.as_deref().unwrap_or(DEFAULT_AGENT_SPEC_VERSION);
    if version != DEFAULT_AGENT_SPEC_VERSION {
        return Err(crate::exception::OctopusError::Other(format!(
            "Unsupported agent spec version: {}",
            version
        )));
    }

    let mut spec = doc.agent;

    // Resolve relative paths against the agent file's parent directory.
    if let Some(ref mut path) = spec.system_prompt_path
        && path.is_relative()
    {
        *path = agent_file.parent().unwrap_or(Path::new(".")).join(&path);
    }
    for sub in spec.subagents.values_mut() {
        if sub.path.is_relative() {
            sub.path = agent_file
                .parent()
                .unwrap_or(Path::new("."))
                .join(&sub.path);
        }
    }

    // Resolve inheritance.
    if let Some(ref extend) = spec.extend {
        let base_file = if extend == "default" {
            default_agent_file()
        } else {
            agent_file.parent().unwrap_or(Path::new(".")).join(extend)
        };
        let mut base = load_raw_agent_spec(&base_file)?;

        if spec.name.is_some() {
            base.name = spec.name;
        }
        if spec.system_prompt_path.is_some() {
            base.system_prompt_path = spec.system_prompt_path;
        }
        // system_prompt_args are merged, not overwritten.
        for (k, v) in spec.system_prompt_args {
            base.system_prompt_args.insert(k, v);
        }
        if spec.model.is_some() {
            base.model = spec.model;
        }
        if spec.when_to_use.is_some() {
            base.when_to_use = spec.when_to_use;
        }
        if spec.tools.is_some() {
            base.tools = spec.tools;
        }
        if spec.allowed_tools.is_some() {
            base.allowed_tools = spec.allowed_tools;
        }
        if !spec.exclude_tools.is_empty() {
            base.exclude_tools = spec.exclude_tools;
        }
        if !spec.subagents.is_empty() {
            base.subagents = spec.subagents;
        }
        spec = base;
    }

    Ok(spec)
}

/// Parse raw tool name strings into [`BuiltinTool`] variants.
///
/// Unknown or not-yet-ported tools are logged and skipped rather than failing
/// the entire agent load.
fn parse_builtin_tools(names: Vec<String>) -> Vec<BuiltinTool> {
    names
        .into_iter()
        .filter_map(|s| match s.parse::<BuiltinTool>() {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!("Skipping unknown tool in agent spec: {}", e);
                None
            }
        })
        .collect()
}
