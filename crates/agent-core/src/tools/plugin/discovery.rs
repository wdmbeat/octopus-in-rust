use std::collections::HashSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use extism::{CompiledPlugin, Manifest, Plugin, PluginBuilder, Wasm};
use llm_provider::tooling::CallableTool;
use serde_json::Value;

use crate::tools::plugin::manifest::{PluginManifest, PluginMetadata, default_schema};

/// A known export of the Brain WASM plugin ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginExport {
    /// Execute the tool with a JSON payload.
    Execute,
    /// Register one or more tools (modern ABI).
    RegisterTools,
    /// Legacy metadata export.
    ToolMetadata,
}

impl PluginExport {
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginExport::Execute => "execute",
            PluginExport::RegisterTools => "register_tools",
            PluginExport::ToolMetadata => "tool_metadata",
        }
    }
}

impl std::fmt::Display for PluginExport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tool backed by a WebAssembly plugin using the Extism runtime.
#[derive(Clone)]
pub struct WasmPluginTool {
    name: String,
    description: String,
    prompt_fragment: Option<String>,
    schema: Value,
    compiled: CompiledPlugin,
}

#[async_trait]
impl CallableTool for WasmPluginTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    fn prompt_fragment(&self) -> Option<&str> {
        self.prompt_fragment.as_deref()
    }

    async fn call_raw(&self, arguments: Value) -> llm_provider::tooling::ToolReturnValue {
        let input = serde_json::json!({
            "tool": self.name,
            "arguments": arguments,
        })
        .to_string();

        let compiled = self.compiled.clone();

        let output = match tokio::task::spawn_blocking(move || {
            let mut plugin = match Plugin::new_from_compiled(&compiled) {
                Ok(p) => p,
                Err(e) => return Err(format!("Failed to instantiate plugin: {}", e)),
            };

            plugin
                .call::<&str, &str>(PluginExport::Execute.as_str(), &input)
                .map_err(|e| format!("Plugin execution error: {}", e))
                .map(|s| s.to_string())
        })
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return llm_provider::tooling::ToolReturnValue::error(e),
            Err(e) => {
                return llm_provider::tooling::ToolReturnValue::error(format!(
                    "Plugin task panicked: {}",
                    e
                ));
            }
        };

        llm_provider::tooling::ToolReturnValue::ok(output)
    }
}

unsafe impl Send for WasmPluginTool {}
unsafe impl Sync for WasmPluginTool {}

/// Discover and load all WASM plugins from the given directory.
///
/// Scans for `.wasm` files and attempts to load each one as a plugin tool.
/// Failures are logged as warnings and skipped.
///
/// Returns a list of `(source_label, tool)` pairs. The source label is the
/// plugin file stem (e.g. `faf_units_plugin`), which lets callers group tools
/// by the plugin that exposed them.
pub fn discover_plugins(
    plugins_dir: &Path,
) -> Vec<(String, Box<dyn llm_provider::tooling::CallableTool>)> {
    discover_plugins_filtered(plugins_dir, None)
}

fn discover_plugins_filtered(
    plugins_dir: &Path,
    allowed_names: Option<&HashSet<String>>,
) -> Vec<(String, Box<dyn llm_provider::tooling::CallableTool>)> {
    let mut tools = Vec::new();

    if !plugins_dir.is_dir() {
        return tools;
    }

    let entries = match std::fs::read_dir(plugins_dir) {
        Ok(e) => e,
        Err(_) => return tools,
    };

    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();

        if path.extension() != Some(std::ffi::OsStr::new("wasm")) {
            continue;
        }

        let source_label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        if let Some(allowed) = allowed_names
            && !allowed.contains(&source_label)
        {
            continue;
        }

        match load_tools_from_wasm(&path) {
            Ok(plugin_tools) => {
                for tool in plugin_tools {
                    tracing::info!(
                        "Loaded WASM plugin tool: {} (from {})",
                        tool.name(),
                        source_label
                    );
                    tools.push((source_label.clone(), tool));
                }
            }
            Err(e) => {
                tracing::warn!("Failed to load WASM plugin '{}': {}", path.display(), e);
            }
        }
    }

    tools
}

fn build_extism_manifest(wasm_bytes: &[u8], plugin_manifest: Option<&PluginManifest>) -> Manifest {
    let mut manifest = Manifest::new([Wasm::data(wasm_bytes.to_vec())]);

    if let Some(pm) = plugin_manifest {
        if let Some(ref hosts) = pm.allowed_hosts {
            manifest = manifest.with_allowed_hosts(hosts.iter().cloned());
        } else {
            manifest = manifest.disallow_all_hosts();
        }

        if let Some(ref paths) = pm.allowed_paths {
            manifest =
                manifest.with_allowed_paths(paths.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        if let Some(pages) = pm.max_memory_pages {
            manifest = manifest.with_memory_max(pages);
        }

        if let Some(ms) = pm.timeout_ms {
            manifest = manifest.with_timeout(std::time::Duration::from_millis(ms));
        }
    } else {
        manifest = manifest.disallow_all_hosts();
    }

    manifest
}

fn load_metadata_from_register_tools(
    compiled: &CompiledPlugin,
    path: &Path,
) -> Result<Vec<PluginMetadata>, String> {
    let mut plugin = Plugin::new_from_compiled(compiled)
        .map_err(|e| format!("Failed to instantiate plugin for register_tools: {}", e))?;

    let export = PluginExport::RegisterTools;
    if !plugin.function_exists(export.as_str()) {
        return Err(format!("Plugin does not export '{}'", export));
    }

    let json = plugin
        .call::<&str, &str>(export.as_str(), "")
        .map_err(|e| format!("{} call failed: {}", export, e))?;

    let defs: Vec<crate::tools::plugin::manifest::ToolDef> =
        serde_json::from_str(json).map_err(|e| {
            format!(
                "Invalid register_tools JSON from '{}': {}",
                path.display(),
                e
            )
        })?;

    if defs.is_empty() {
        return Err(format!(
            "register_tools returned empty list for '{}'",
            path.display()
        ));
    }

    Ok(defs
        .into_iter()
        .map(|def| PluginMetadata {
            name: def.name,
            description: def.description,
            prompt_fragment: def.prompt_fragment,
            schema: def.parameters,
        })
        .collect())
}

fn load_metadata_from_plugin(
    compiled: &CompiledPlugin,
    path: &Path,
) -> Result<PluginMetadata, String> {
    let mut plugin = Plugin::new_from_compiled(compiled)
        .map_err(|e| format!("Failed to instantiate plugin for metadata: {}", e))?;

    let export = PluginExport::ToolMetadata;
    if !plugin.function_exists(export.as_str()) {
        return Err(format!("Plugin does not export '{}'", export));
    }

    let json = plugin
        .call::<&str, &str>(export.as_str(), "")
        .map_err(|e| format!("{} call failed: {}", export, e))?;

    serde_json::from_str(json).map_err(|e| {
        format!(
            "Invalid tool_metadata JSON from '{}': {}",
            path.display(),
            e
        )
    })
}

fn fallback_metadata(path: &Path) -> PluginMetadata {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    PluginMetadata {
        name: name.clone(),
        description: format!("WASM plugin tool: {}", name),
        prompt_fragment: None,
        schema: default_schema(),
    }
}

/// Minimal metadata about a loaded WASM plugin tool, suitable for UIs and status output.
#[derive(Debug, Clone)]
pub struct WasmToolInfo {
    pub name: String,
    pub description: String,
}

/// Load a single WASM plugin file and return the first tool it exposes.
///
/// This is the public entry point used by tooling such as the `qqbot` CLI to
/// validate a `.wasm` file before registering it. Plugins that expose multiple
/// tools should use [`load_tools_from_wasm`] to obtain every tool.
pub fn load_tool_from_wasm(path: &Path) -> Result<Box<dyn CallableTool>, String> {
    load_tools_from_wasm(path)?
        .into_iter()
        .next()
        .ok_or_else(|| format!("WASM plugin '{}' exposed no tools", path.display()))
}

/// Load a single WASM plugin file and return every tool it exposes.
///
/// Modern Brain plugins export `register_tools`, which may return multiple tool
/// definitions. This function compiles the plugin once and returns one
/// `CallableTool` handle per definition, all sharing the same compiled plugin.
pub fn load_tools_from_wasm(path: &Path) -> Result<Vec<Box<dyn CallableTool>>, String> {
    let wasm_bytes = std::fs::read(path).map_err(|e| format!("Failed to read WASM file: {}", e))?;

    let manifest_path = path.with_extension("json");
    let plugin_manifest: Option<PluginManifest> = if manifest_path.is_file() {
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("Failed to read manifest: {}", e))?;
        Some(serde_json::from_str(&text).map_err(|e| format!("Invalid manifest JSON: {}", e))?)
    } else {
        None
    };

    let extism_manifest = build_extism_manifest(&wasm_bytes, plugin_manifest.as_ref());

    let compiled = PluginBuilder::new(extism_manifest)
        .with_wasi(true)
        .compile()
        .map_err(|e| format!("Failed to compile WASM plugin: {}", e))?;

    let metadata_list: Vec<PluginMetadata> = if let Some(pm) = plugin_manifest {
        vec![pm.into_metadata()]
    } else {
        // Prefer the Brain tool ABI; fall back to the older `tool_metadata` export.
        load_metadata_from_register_tools(&compiled, path)
            .or_else(|_| load_metadata_from_plugin(&compiled, path).map(|meta| vec![meta]))
            .unwrap_or_else(|_| vec![fallback_metadata(path)])
    };

    Ok(metadata_list
        .into_iter()
        .map(|metadata| {
            Box::new(WasmPluginTool {
                name: metadata.name,
                description: metadata.description,
                prompt_fragment: metadata.prompt_fragment,
                schema: metadata.schema,
                compiled: compiled.clone(),
            }) as Box<dyn CallableTool>
        })
        .collect())
}

/// Inspect a WASM plugin file and return its tool metadata without keeping the
/// compiled plugin alive.
pub fn inspect_wasm_plugin(path: &Path) -> Result<WasmToolInfo, String> {
    let tool = load_tool_from_wasm(path)?;
    Ok(WasmToolInfo {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
    })
}

/// Discover and return metadata for every loadable WASM plugin in a directory.
///
/// Invalid plugins are skipped (with tracing warnings) rather than failing the
/// whole scan. This is useful for status output and quick inventory.
pub fn discover_plugin_infos(plugins_dir: &Path) -> Vec<WasmToolInfo> {
    discover_plugins(plugins_dir)
        .into_iter()
        .map(|(_source, tool)| WasmToolInfo {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
        })
        .collect()
}

/// Default plugins directory path (`~/.kimi/plugins`).
pub fn default_plugins_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".kimi/plugins"))
}

/// A [`ToolSource`] that discovers Extism `.wasm` plugins in a directory.
#[derive(Debug, Clone)]
pub struct ExtismPluginSource {
    plugins_dir: PathBuf,
    allowed_names: Option<HashSet<String>>,
}

impl ExtismPluginSource {
    /// Create a source that scans `plugins_dir` for `.wasm` files.
    pub fn new(plugins_dir: impl Into<PathBuf>) -> Self {
        Self {
            plugins_dir: plugins_dir.into(),
            allowed_names: None,
        }
    }

    /// Create a source that loads only plugins whose file stem is in `allowed_names`.
    pub fn with_filter(
        plugins_dir: impl Into<PathBuf>,
        allowed_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            plugins_dir: plugins_dir.into(),
            allowed_names: Some(allowed_names.into_iter().map(Into::into).collect()),
        }
    }
}

impl crate::core::registry::ToolSource for ExtismPluginSource {
    fn name(&self) -> &str {
        "extism-plugins"
    }

    fn load_tools(&self) -> Vec<(String, Box<dyn llm_provider::tooling::CallableTool>)> {
        discover_plugins_filtered(&self.plugins_dir, self.allowed_names.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qqbot_plugins_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("data")
            .join("qqbot-data")
            .join("plugins")
    }

    #[test]
    fn test_discover_faf_units_plugin() {
        let dir = qqbot_plugins_dir();
        if !dir.join("faf_units_plugin.wasm").exists() {
            eprintln!(
                "Skipping test: faf_units_plugin.wasm not found at {}",
                dir.display()
            );
            return;
        }

        let tools = discover_plugins(&dir);
        let names: Vec<&str> = tools.iter().map(|(_source, t)| t.name()).collect();
        for expected in [
            "faf_units_search",
            "faf_units_get",
            "faf_units_compare",
            "faf_units_naive_dps",
        ] {
            assert!(
                names.contains(&expected),
                "expected {} plugin tool, got {:?}",
                expected,
                names
            );
        }

        // Verify each faf_units tool is tagged with the plugin file stem.
        for (source, tool) in &tools {
            if tool.name().starts_with("faf_units_") {
                assert_eq!(source, "faf_units_plugin");
            }
        }
    }

    #[tokio::test]
    async fn test_faf_units_plugin_tool_execution() {
        let dir = qqbot_plugins_dir();
        let wasm_path = dir.join("faf_units_plugin.wasm");
        if !wasm_path.exists() {
            eprintln!("Skipping test: faf_units_plugin.wasm not found");
            return;
        }

        let tools = discover_plugins(&dir);
        let (_source, tool) = tools
            .into_iter()
            .find(|(_source, t)| t.name() == "faf_units_search")
            .expect("faf_units plugin tool should be loaded");

        let args = serde_json::json!({
            "query": "UEF tech1 tank",
            "limit": 5
        });

        let result = tool.call_raw(args).await;
        assert!(!result.is_error, "tool should succeed: {:?}", result);
        let output = result.output.unwrap().as_str().unwrap().to_string();
        assert!(
            output.contains("UEL0201"),
            "expected UEL0201 in search results: {output}"
        );
    }

    #[tokio::test]
    async fn test_faf_units_plugin_chinese_search() {
        let dir = qqbot_plugins_dir();
        let wasm_path = dir.join("faf_units_plugin.wasm");
        if !wasm_path.exists() {
            eprintln!("Skipping test: faf_units_plugin.wasm not found");
            return;
        }

        let tools = discover_plugins(&dir);
        let (_source, tool) = tools
            .into_iter()
            .find(|(_source, t)| t.name() == "faf_units_search")
            .expect("faf_units plugin tool should be loaded");

        // Chinese players often use generic type names like "中型坦克" (medium tank).
        let args = serde_json::json!({
            "query": "中型坦克",
            "limit": 5
        });

        let result = tool.call_raw(args).await;
        assert!(!result.is_error, "tool should succeed: {:?}", result);
        let output = result.output.unwrap().as_str().unwrap().to_string();
        assert!(
            output.contains("UEL0201"),
            "expected UEL0201 in Chinese search results: {output}"
        );
        assert!(
            output.contains("MA12攻击者"),
            "expected Chinese name in search results: {output}"
        );
    }
}
