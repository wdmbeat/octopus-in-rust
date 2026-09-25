use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::soul::approval::ApprovalMode;

const STATE_FILE_NAME: &str = "state.json";
const LEGACY_METADATA_FILENAME: &str = "metadata.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApprovalStateData {
    #[serde(default)]
    pub mode: ApprovalMode,
    #[serde(default)]
    pub auto_approve_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItemState {
    pub title: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default = "default_version")]
    pub version: i32,
    #[serde(default)]
    pub approval: ApprovalStateData,
    #[serde(default)]
    pub additional_dirs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub title_generated: bool,
    #[serde(default)]
    pub title_generate_attempts: i32,
    #[serde(default)]
    pub plan_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_mtime: Option<f64>,
    #[serde(default)]
    pub archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<f64>,
    #[serde(default)]
    pub auto_archive_exempt: bool,
    #[serde(default)]
    pub todos: Vec<TodoItemState>,
}

fn default_version() -> i32 {
    1
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            version: 1,
            approval: ApprovalStateData::default(),
            additional_dirs: Vec::new(),
            custom_title: None,
            title_generated: false,
            title_generate_attempts: 0,
            plan_mode: false,
            plan_session_id: None,
            plan_slug: None,
            wire_mtime: None,
            archived: false,
            archived_at: None,
            auto_archive_exempt: false,
            todos: Vec::new(),
        }
    }
}

fn migrate_legacy_metadata<'a>(session_dir: &'a Path, state: &'a mut SessionState) -> &'a str {
    let metadata_file = session_dir.join(LEGACY_METADATA_FILENAME);
    if !metadata_file.exists() {
        return "skip";
    }
    let data: serde_json::Value = match std::fs::read_to_string(&metadata_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(d) => d,
        None => return "skip",
    };

    let mut changed = false;

    if state.custom_title.is_none()
        && let Some(title) = data.get("title").and_then(|v| v.as_str())
        && !title.is_empty()
        && title != "Untitled"
    {
        state.custom_title = Some(title.to_string());
        changed = true;
    }
    if !state.title_generated
        && let Some(true) = data.get("title_generated").and_then(|v| v.as_bool())
    {
        state.title_generated = true;
        changed = true;
    }
    if state.title_generate_attempts == 0
        && let Some(n) = data.get("title_generate_attempts").and_then(|v| v.as_i64())
        && n > 0
    {
        state.title_generate_attempts = n as i32;
        changed = true;
    }
    if !state.archived
        && let Some(true) = data.get("archived").and_then(|v| v.as_bool())
    {
        state.archived = true;
        changed = true;
    }
    if state.archived_at.is_none()
        && let Some(n) = data.get("archived_at").and_then(|v| v.as_f64())
    {
        state.archived_at = Some(n);
        changed = true;
    }
    if !state.auto_archive_exempt
        && let Some(true) = data.get("auto_archive_exempt").and_then(|v| v.as_bool())
    {
        state.auto_archive_exempt = true;
        changed = true;
    }
    if state.wire_mtime.is_none()
        && let Some(n) = data.get("wire_mtime").and_then(|v| v.as_f64())
    {
        state.wire_mtime = Some(n);
        changed = true;
    }

    if changed { "migrated" } else { "no_change" }
}

pub fn load_session_state(session_dir: &Path) -> SessionState {
    let state_file = session_dir.join(STATE_FILE_NAME);
    let mut state = if state_file.exists() {
        std::fs::read_to_string(&state_file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        SessionState::default()
    };

    let migration = migrate_legacy_metadata(session_dir, &mut state);
    if migration == "migrated" || migration == "no_change" {
        if migration == "migrated" {
            let _ = save_session_state(&state, session_dir);
        }
        let _ = std::fs::remove_file(session_dir.join(LEGACY_METADATA_FILENAME));
    }

    state
}

pub fn save_session_state(state: &SessionState, session_dir: &Path) -> std::io::Result<()> {
    let state_file = session_dir.join(STATE_FILE_NAME);
    let content = serde_json::to_string_pretty(state)?;
    std::fs::write(state_file, content)
}
