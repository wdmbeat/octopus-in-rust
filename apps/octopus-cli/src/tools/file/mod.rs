use std::path::{Path, PathBuf};

use async_trait::async_trait;
use llm_provider::tooling::{CallableTool2, ToolReturnValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_LINES: usize = 1000;
const MAX_LINE_LENGTH: usize = 2000;
const _MAX_BYTES: usize = 100 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileParams {
    pub path: String,
    #[serde(default = "default_line_offset")]
    pub line_offset: i32,
    #[serde(default = "default_n_lines")]
    pub n_lines: usize,
}

fn default_line_offset() -> i32 {
    1
}

fn default_n_lines() -> usize {
    MAX_LINES
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WriteFileParams {
    pub path: String,
    pub content: String,
    #[serde(default = "default_write_mode")]
    pub mode: String,
}

fn default_write_mode() -> String {
    "overwrite".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edit {
    pub old: String,
    pub new: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StrReplaceFileParams {
    pub path: String,
    pub edit: Edit,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GlobParams {
    pub pattern: String,
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default = "default_include_dirs")]
    pub include_dirs: bool,
}

fn default_include_dirs() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GrepParams {
    pub pattern: String,
    #[serde(default = "default_grep_path")]
    pub path: String,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default = "default_output_mode")]
    pub output_mode: String,
    #[serde(default)]
    pub before_context: Option<usize>,
    #[serde(default)]
    pub after_context: Option<usize>,
    #[serde(default)]
    pub context: Option<usize>,
    #[serde(default = "default_line_number")]
    pub line_number: bool,
    #[serde(default)]
    pub ignore_case: bool,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default = "default_head_limit")]
    pub head_limit: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub include_ignored: bool,
}

fn default_grep_path() -> String {
    ".".to_string()
}

fn default_output_mode() -> String {
    "files_with_matches".to_string()
}

fn default_line_number() -> bool {
    true
}

fn default_head_limit() -> usize {
    250
}

pub struct ReadFileTool;
pub struct WriteFileTool;
pub struct StrReplaceFileTool;
pub struct GlobTool;
pub struct GrepTool;

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StrReplaceFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl StrReplaceFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

fn resolve_path(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

fn read_file_lines(path: &Path, line_offset: i32, n_lines: usize) -> Result<String, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start = if line_offset < 0 {
        let tail_count = line_offset.unsigned_abs() as usize;
        total_lines.saturating_sub(tail_count)
    } else {
        (line_offset as usize).saturating_sub(1)
    };

    let end = (start + n_lines).min(total_lines).min(start + MAX_LINES);

    let mut result = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let truncated = if line.len() > MAX_LINE_LENGTH {
            &line[..MAX_LINE_LENGTH]
        } else {
            line
        };
        result.push_str(&format!("{:6}\t{}\n", line_num, truncated));
    }

    let mut msg = format!(
        "{} lines read from file starting from line {}.",
        end - start,
        start + 1
    );
    msg.push_str(&format!(" Total lines in file: {}.", total_lines));
    if end < total_lines && end - start >= MAX_LINES {
        msg.push_str(&format!(" Max {} lines reached.", MAX_LINES));
    } else if end < total_lines {
        msg.push_str(" End of file reached.");
    }

    Ok(result)
}

#[async_trait]
impl CallableTool2 for ReadFileTool {
    type Params = ReadFileParams;

    fn name(&self) -> &str {
        "ReadFile"
    }

    fn description(&self) -> &str {
        "Read a file from the local filesystem."
    }

    async fn call_typed(&self, params: ReadFileParams) -> ToolReturnValue {
        let path = resolve_path(&params.path);

        if !path.exists() {
            return ToolReturnValue::error(format!("`{}` does not exist.", params.path));
        }
        if !path.is_file() {
            return ToolReturnValue::error(format!("`{}` is not a file.", params.path));
        }

        match read_file_lines(&path, params.line_offset, params.n_lines) {
            Ok(result) => ToolReturnValue::ok(result),
            Err(e) => ToolReturnValue::error(e),
        }
    }
}

#[async_trait]
impl CallableTool2 for WriteFileTool {
    type Params = WriteFileParams;

    fn name(&self) -> &str {
        "WriteFile"
    }

    fn description(&self) -> &str {
        "Write content to a file."
    }

    async fn call_typed(&self, params: WriteFileParams) -> ToolReturnValue {
        let path = resolve_path(&params.path);

        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return ToolReturnValue::error(format!("Failed to create parent directory: {}", e));
        }

        match params.mode.as_str() {
            "overwrite" => {
                if let Err(e) = std::fs::write(&path, &params.content) {
                    return ToolReturnValue::error(format!("Failed to write file: {}", e));
                }
            }
            "append" => {
                use std::io::Write;
                let mut file = match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    Ok(f) => f,
                    Err(e) => return ToolReturnValue::error(format!("Failed to open file: {}", e)),
                };
                if let Err(e) = file.write_all(params.content.as_bytes()) {
                    return ToolReturnValue::error(format!("Failed to append to file: {}", e));
                }
            }
            _ => return ToolReturnValue::error(format!("Invalid mode: {}", params.mode)),
        }

        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        ToolReturnValue::ok(format!(
            "File successfully written. Current size: {} bytes.",
            size
        ))
    }
}

#[async_trait]
impl CallableTool2 for StrReplaceFileTool {
    type Params = StrReplaceFileParams;

    fn name(&self) -> &str {
        "StrReplaceFile"
    }

    fn description(&self) -> &str {
        "Replace text in a file."
    }

    async fn call_typed(&self, params: StrReplaceFileParams) -> ToolReturnValue {
        let path = resolve_path(&params.path);

        if !path.exists() {
            return ToolReturnValue::error(format!("`{}` does not exist.", params.path));
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolReturnValue::error(format!("Failed to read file: {}", e)),
        };

        let new_content = if params.edit.replace_all {
            content.replace(&params.edit.old, &params.edit.new)
        } else {
            content.replacen(&params.edit.old, &params.edit.new, 1)
        };

        if new_content == content {
            return ToolReturnValue::error(
                "No replacements were made. The old string was not found in the file.",
            );
        }

        if let Err(e) = std::fs::write(&path, &new_content) {
            return ToolReturnValue::error(format!("Failed to write file: {}", e));
        }

        ToolReturnValue::ok("File successfully edited.")
    }
}

#[async_trait]
impl CallableTool2 for GlobTool {
    type Params = GlobParams;

    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern."
    }

    async fn call_typed(&self, params: GlobParams) -> ToolReturnValue {
        if params.pattern.starts_with("**") {
            return ToolReturnValue::error(
                "Pattern starting with '**' is not allowed. Use a more specific pattern.",
            );
        }

        let dir = params
            .directory
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let pattern = dir.join(&params.pattern);
        let pattern_str = pattern.to_string_lossy();

        let entries = match glob::glob(&pattern_str) {
            Ok(e) => e,
            Err(e) => return ToolReturnValue::error(format!("Invalid glob pattern: {}", e)),
        };

        let mut matches = Vec::new();
        for entry in entries {
            if let Ok(path) = entry
                && (params.include_dirs || path.is_file())
            {
                matches.push(path.to_string_lossy().to_string());
            }
        }

        matches.sort();

        if matches.len() > 1000 {
            matches.truncate(1000);
        }

        ToolReturnValue::ok(matches.join("\n"))
    }
}

#[async_trait]
impl CallableTool2 for GrepTool {
    type Params = GrepParams;

    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search file contents using regex."
    }

    async fn call_typed(&self, params: GrepParams) -> ToolReturnValue {
        let path = resolve_path(&params.path);

        let mut args = vec!["rg".to_string()];

        if params.ignore_case {
            args.push("--ignore-case".to_string());
        }
        if params.multiline {
            args.push("--multiline".to_string());
            args.push("--multiline-dotall".to_string());
        }

        match params.output_mode.as_str() {
            "content" => {
                if let Some(b) = params.before_context {
                    args.push("--before-context".to_string());
                    args.push(b.to_string());
                }
                if let Some(a) = params.after_context {
                    args.push("--after-context".to_string());
                    args.push(a.to_string());
                }
                if let Some(c) = params.context {
                    args.push("--context".to_string());
                    args.push(c.to_string());
                }
                if params.line_number {
                    args.push("--line-number".to_string());
                }
            }
            "files_with_matches" => {
                args.push("--files-with-matches".to_string());
            }
            "count_matches" => {
                args.push("--count-matches".to_string());
            }
            _ => {}
        }

        if let Some(glob) = params.glob {
            args.push("--glob".to_string());
            args.push(glob);
        }
        if let Some(ty) = params.r#type {
            args.push("--type".to_string());
            args.push(ty);
        }

        if params.include_ignored {
            args.push("--no-ignore".to_string());
        }
        args.push("--hidden".to_string());

        args.push("--".to_string());
        args.push(params.pattern);
        args.push(path.to_string_lossy().to_string());

        let output = match tokio::process::Command::new("rg")
            .args(&args[1..])
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => return ToolReturnValue::error(format!("Failed to run grep: {}", e)),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines: Vec<&str> = stdout.lines().collect();

        if params.offset > 0 {
            lines = lines.into_iter().skip(params.offset).collect();
        }

        if params.head_limit > 0 && lines.len() > params.head_limit {
            lines.truncate(params.head_limit);
        }

        ToolReturnValue::ok(lines.join("\n"))
    }
}
