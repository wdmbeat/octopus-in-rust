use std::path::{Path, PathBuf};

use crate::metadata::{WorkDirMeta, load_metadata, save_metadata};
use crate::session_state::{SessionState, load_session_state, save_session_state};

fn migrate_session_context_file(work_dir_meta: &WorkDirMeta, session_id: &str) {
    let old = work_dir_meta
        .sessions_dir()
        .join(format!("{}.jsonl", session_id));
    let new = work_dir_meta
        .sessions_dir()
        .join(session_id)
        .join("context.jsonl");
    if old.exists() && !new.exists() {
        let _ = std::fs::create_dir_all(new.parent().unwrap());
        let _ = std::fs::rename(&old, &new);
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub work_dir: PathBuf,
    pub work_dir_meta: WorkDirMeta,
    pub context_file: PathBuf,
    pub wire_file_path: PathBuf,
    pub state: SessionState,
    pub title: String,
    pub updated_at: f64,
}

impl Session {
    pub fn dir(&self) -> PathBuf {
        let path = self.work_dir_meta.sessions_dir().join(&self.id);
        let _ = std::fs::create_dir_all(&path);
        path
    }

    pub fn subagents_dir(&self) -> PathBuf {
        let path = self.dir().join("subagents");
        let _ = std::fs::create_dir_all(&path);
        path
    }

    pub fn is_empty(&self) -> bool {
        if self.state.custom_title.is_some() {
            return false;
        }
        if self.wire_file_path.exists()
            && let Ok(meta) = std::fs::metadata(&self.wire_file_path)
            && meta.len() > 0
        {
            return false;
        }
        match std::fs::read_to_string(&self.context_file) {
            Ok(content) => {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line)
                        && let Some(role) = obj.get("role").and_then(|v| v.as_str())
                        && !role.starts_with('_')
                    {
                        return false;
                    }
                }
            }
            Err(_) => return true,
        }
        true
    }

    pub fn save_state(&mut self) {
        let fresh = load_session_state(&self.dir());
        self.state.custom_title = fresh.custom_title;
        self.state.title_generated = fresh.title_generated;
        self.state.title_generate_attempts = fresh.title_generate_attempts;
        self.state.archived = fresh.archived;
        self.state.archived_at = fresh.archived_at;
        self.state.auto_archive_exempt = fresh.auto_archive_exempt;
        save_session_state(&self.state, &self.dir()).ok();
    }

    pub async fn delete(&self) {
        let session_dir = self.work_dir_meta.sessions_dir().join(&self.id);
        if session_dir.exists() {
            let _ = tokio::fs::remove_dir_all(&session_dir).await;
        }
    }

    pub async fn refresh(&mut self) {
        self.title = "Untitled".to_string();
        self.updated_at = if self.context_file.exists() {
            tokio::fs::metadata(&self.context_file)
                .await
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        } else {
            0.0
        };

        if let Some(ref custom) = self.state.custom_title {
            self.title = custom.clone();
            return;
        }

        if self.wire_file_path.exists()
            && let Ok(content) = tokio::fs::read_to_string(&self.wire_file_path).await
        {
            for line in content.lines() {
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line)
                    && let Some(input) = obj.get("user_input").and_then(|v| v.as_str())
                {
                    self.title = input.chars().take(50).collect();
                    return;
                }
            }
        }
    }

    pub async fn create(work_dir: &Path, session_id: Option<String>) -> std::io::Result<Session> {
        let work_dir = work_dir
            .canonicalize()
            .unwrap_or_else(|_| work_dir.to_path_buf());

        let mut metadata = load_metadata();
        let work_dir_meta = metadata
            .get_work_dir_meta(&work_dir)
            .cloned()
            .unwrap_or_else(|| {
                let meta = WorkDirMeta {
                    path: work_dir.to_string_lossy().to_string(),
                    kaos: "local".to_string(),
                    last_session_id: None,
                };
                metadata.work_dirs.push(meta.clone());
                meta
            });

        let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let session_dir = work_dir_meta.sessions_dir().join(&session_id);
        std::fs::create_dir_all(&session_dir)?;

        let context_file = session_dir.join("context.jsonl");
        if context_file.exists() {
            std::fs::remove_file(&context_file)?;
        }
        std::fs::File::create(&context_file)?;

        save_metadata(&metadata);

        let mut session = Session {
            id: session_id,
            work_dir,
            work_dir_meta,
            context_file,
            wire_file_path: session_dir.join("wire.jsonl"),
            state: SessionState::default(),
            title: String::new(),
            updated_at: 0.0,
        };
        session.refresh().await;
        Ok(session)
    }

    pub async fn find(work_dir: &Path, session_id: &str) -> Option<Session> {
        let work_dir = work_dir
            .canonicalize()
            .unwrap_or_else(|_| work_dir.to_path_buf());
        let metadata = load_metadata();
        let work_dir_meta = metadata.get_work_dir_meta(&work_dir)?.clone();

        migrate_session_context_file(&work_dir_meta, session_id);

        let session_dir = work_dir_meta.sessions_dir().join(session_id);
        if !session_dir.is_dir() {
            return None;
        }

        let context_file = session_dir.join("context.jsonl");
        if !context_file.exists() {
            return None;
        }

        let mut session = Session {
            id: session_id.to_string(),
            work_dir,
            work_dir_meta,
            context_file,
            wire_file_path: session_dir.join("wire.jsonl"),
            state: load_session_state(&session_dir),
            title: String::new(),
            updated_at: 0.0,
        };
        session.refresh().await;
        Some(session)
    }

    pub async fn list(work_dir: &Path) -> Vec<Session> {
        let work_dir = work_dir
            .canonicalize()
            .unwrap_or_else(|_| work_dir.to_path_buf());
        let metadata = load_metadata();
        let work_dir_meta = match metadata.get_work_dir_meta(&work_dir) {
            Some(m) => m.clone(),
            None => return Vec::new(),
        };

        let mut session_ids = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(work_dir_meta.sessions_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let id = if path.is_dir() {
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                } else {
                    None
                };
                if let Some(id) = id {
                    session_ids.insert(id);
                }
            }
        }

        let mut sessions = Vec::new();
        for session_id in session_ids {
            migrate_session_context_file(&work_dir_meta, &session_id);
            let session_dir = work_dir_meta.sessions_dir().join(&session_id);
            if !session_dir.is_dir() {
                continue;
            }
            let context_file = session_dir.join("context.jsonl");
            if !context_file.exists() {
                continue;
            }
            let mut session = Session {
                id: session_id,
                work_dir: work_dir.clone(),
                work_dir_meta: work_dir_meta.clone(),
                context_file,
                wire_file_path: session_dir.join("wire.jsonl"),
                state: load_session_state(&session_dir),
                title: String::new(),
                updated_at: 0.0,
            };
            if session.is_empty() {
                continue;
            }
            session.refresh().await;
            sessions.push(session);
        }

        sessions.sort_by(|a, b| b.updated_at.partial_cmp(&a.updated_at).unwrap());
        sessions
    }

    pub async fn list_all() -> Vec<Session> {
        let metadata = load_metadata();
        let mut all_sessions = Vec::new();
        for wd in &metadata.work_dirs {
            let path = PathBuf::from(&wd.path);
            let sessions = Session::list(&path).await;
            all_sessions.extend(sessions);
        }
        all_sessions.sort_by(|a, b| b.updated_at.partial_cmp(&a.updated_at).unwrap());
        all_sessions
    }

    pub async fn continue_(work_dir: &Path) -> Option<Session> {
        let work_dir = work_dir
            .canonicalize()
            .unwrap_or_else(|_| work_dir.to_path_buf());
        let metadata = load_metadata();
        let work_dir_meta = metadata.get_work_dir_meta(&work_dir)?;
        let last_session_id = work_dir_meta.last_session_id.as_ref()?;
        Session::find(&work_dir, last_session_id).await
    }
}
