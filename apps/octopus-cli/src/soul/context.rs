use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::soul::message::system;
use crate::wire::Message;

pub struct Context {
    file_backend: PathBuf,
    history: Vec<Message>,
    token_count: usize,
    pending_token_estimate: usize,
    next_checkpoint_id: usize,
    system_prompt: Option<String>,
}

impl Context {
    pub fn new(file_backend: PathBuf) -> Self {
        Self {
            file_backend,
            history: Vec::new(),
            token_count: 0,
            pending_token_estimate: 0,
            next_checkpoint_id: 0,
            system_prompt: None,
        }
    }

    pub fn restore_sync(&mut self) -> std::io::Result<bool> {
        if !self.history.is_empty() {
            return Err(std::io::Error::other("Context storage is already modified"));
        }
        if !self.file_backend.exists() {
            return Ok(false);
        }
        let meta = std::fs::metadata(&self.file_backend)?;
        if meta.len() == 0 {
            return Ok(false);
        }

        let content = std::fs::read_to_string(&self.file_backend)?;
        let mut messages_after_last_usage: Vec<Message> = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some(line_json) = Self::parse_context_line(line) else {
                continue;
            };
            self.apply_context_record(&line_json, &mut messages_after_last_usage);
        }

        self.pending_token_estimate = estimate_text_tokens(&messages_after_last_usage);
        Ok(true)
    }

    pub async fn restore(&mut self) -> std::io::Result<bool> {
        if !self.history.is_empty() {
            return Err(std::io::Error::other("Context storage is already modified"));
        }
        if !self.file_backend.exists() {
            return Ok(false);
        }
        let meta = fs::metadata(&self.file_backend).await?;
        if meta.len() == 0 {
            return Ok(false);
        }

        let file = fs::File::open(&self.file_backend).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut messages_after_last_usage: Vec<Message> = Vec::new();
        let mut _line_no = 0;

        while let Ok(Some(line)) = lines.next_line().await {
            _line_no += 1;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some(line_json) = Self::parse_context_line(line) else {
                continue;
            };
            self.apply_context_record(&line_json, &mut messages_after_last_usage);
        }

        self.pending_token_estimate = estimate_text_tokens(&messages_after_last_usage);
        Ok(true)
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn token_count(&self) -> usize {
        self.token_count
    }

    pub fn token_count_with_pending(&self) -> usize {
        self.token_count + self.pending_token_estimate
    }

    pub fn n_checkpoints(&self) -> usize {
        self.next_checkpoint_id
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub fn file_backend(&self) -> &Path {
        &self.file_backend
    }

    pub async fn write_system_prompt(&mut self, prompt: &str) -> std::io::Result<()> {
        let prompt_line = format!(
            "{}\n",
            serde_json::json!({"role": "_system_prompt", "content": prompt})
        );

        if !self.file_backend.exists() {
            fs::write(&self.file_backend, &prompt_line).await?;
        } else {
            let meta = fs::metadata(&self.file_backend).await?;
            if meta.len() == 0 {
                fs::write(&self.file_backend, &prompt_line).await?;
            } else {
                let tmp_path = self.file_backend.with_extension("tmp");
                {
                    let mut tmp = fs::File::create(&tmp_path).await?;
                    tmp.write_all(prompt_line.as_bytes()).await?;
                    let mut src = fs::File::open(&self.file_backend).await?;
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        let n = tokio::io::AsyncReadExt::read(&mut src, &mut buf).await?;
                        if n == 0 {
                            break;
                        }
                        tmp.write_all(&buf[..n]).await?;
                    }
                }
                fs::rename(&tmp_path, &self.file_backend).await?;
            }
        }

        self.system_prompt = Some(prompt.to_string());
        Ok(())
    }

    pub async fn checkpoint(&mut self, add_user_message: bool) -> std::io::Result<()> {
        let checkpoint_id = self.next_checkpoint_id;
        self.next_checkpoint_id += 1;

        let line = format!(
            "{}\n",
            serde_json::json!({"role": "_checkpoint", "id": checkpoint_id})
        );
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.file_backend)
            .await?;
        file.write_all(line.as_bytes()).await?;
        drop(file);

        if add_user_message {
            let msg = Message {
                role: "user".to_string(),
                content: vec![system(&format!("CHECKPOINT {checkpoint_id}"))],
                tool_call_id: None,
                tool_calls: None,
            };
            self.append_message(msg).await?;
        }
        Ok(())
    }

    pub async fn revert_to(&mut self, checkpoint_id: usize) -> std::io::Result<()> {
        if checkpoint_id >= self.next_checkpoint_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Checkpoint {checkpoint_id} does not exist"),
            ));
        }

        let rotated_file_path = next_available_rotation(&self.file_backend)
            .ok_or_else(|| std::io::Error::other("No available rotation path found"))?;
        fs::rename(&self.file_backend, &rotated_file_path).await?;

        self.history.clear();
        self.token_count = 0;
        self.next_checkpoint_id = 0;
        self.system_prompt = None;

        let old_file = fs::File::open(&rotated_file_path).await?;
        let old_reader = BufReader::new(old_file);
        let mut old_lines = old_reader.lines();

        let mut new_file = fs::File::create(&self.file_backend).await?;
        let mut messages_after_last_usage: Vec<Message> = Vec::new();

        while let Ok(Some(line)) = old_lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some(line_json) = Self::parse_context_line(trimmed) else {
                continue;
            };
            if line_json.get("role").and_then(|v| v.as_str()) == Some("_checkpoint")
                && line_json.get("id").and_then(|v| v.as_u64()) == Some(checkpoint_id as u64)
            {
                break;
            }
            let keep = self.apply_context_record(&line_json, &mut messages_after_last_usage);
            if keep {
                new_file.write_all(line.as_bytes()).await?;
                new_file.write_all(b"\n").await?;
            }
        }

        self.pending_token_estimate = estimate_text_tokens(&messages_after_last_usage);
        Ok(())
    }

    pub async fn clear(&mut self) -> std::io::Result<()> {
        let rotated_file_path = next_available_rotation(&self.file_backend)
            .ok_or_else(|| std::io::Error::other("No available rotation path found"))?;
        fs::rename(&self.file_backend, &rotated_file_path).await?;
        fs::File::create(&self.file_backend).await?;

        self.history.clear();
        self.token_count = 0;
        self.pending_token_estimate = 0;
        self.next_checkpoint_id = 0;
        self.system_prompt = None;
        Ok(())
    }

    /// Replace the in-memory history and persist it, preserving the system prompt.
    pub async fn replace_history(&mut self, history: Vec<Message>) -> std::io::Result<()> {
        self.history = history.clone();
        self.pending_token_estimate = estimate_text_tokens(&history);

        let mut lines = Vec::new();
        if let Some(prompt) = &self.system_prompt {
            lines.push(format!(
                "{}\n",
                serde_json::json!({"role": "_system_prompt", "content": prompt})
            ));
        }
        for msg in &history {
            lines.push(format!("{}\n", serde_json::to_string(msg).unwrap()));
        }

        fs::write(&self.file_backend, lines.concat()).await?;
        Ok(())
    }

    pub async fn append_message(&mut self, message: impl IntoMessages) -> std::io::Result<()> {
        let messages = message.into_messages();
        self.history.extend(messages.clone());
        self.pending_token_estimate += estimate_text_tokens(&messages);

        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.file_backend)
            .await?;
        for msg in &messages {
            let line = format!("{}\n", serde_json::to_string(msg).unwrap());
            file.write_all(line.as_bytes()).await?;
        }
        Ok(())
    }

    pub async fn update_token_count(&mut self, token_count: usize) -> std::io::Result<()> {
        self.token_count = token_count;
        self.pending_token_estimate = 0;

        let line = format!(
            "{}\n",
            serde_json::json!({"role": "_usage", "token_count": token_count})
        );
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.file_backend)
            .await?;
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }

    fn parse_context_line(line: &str) -> Option<serde_json::Value> {
        serde_json::from_str(line).ok()
    }

    fn apply_context_record(
        &mut self,
        line_json: &serde_json::Value,
        messages_after_last_usage: &mut Vec<Message>,
    ) -> bool {
        let role = line_json.get("role").and_then(|v| v.as_str());
        let Some(role) = role else {
            return false;
        };

        match role {
            "_system_prompt" => {
                if let Some(content) = line_json.get("content").and_then(|v| v.as_str()) {
                    self.system_prompt = Some(content.to_string());
                    return true;
                }
                false
            }
            "_usage" => {
                if let Some(count) = line_json.get("token_count").and_then(|v| v.as_u64()) {
                    self.token_count = count as usize;
                    messages_after_last_usage.clear();
                    return true;
                }
                false
            }
            "_checkpoint" => {
                if let Some(id) = line_json.get("id").and_then(|v| v.as_u64()) {
                    self.next_checkpoint_id = (id as usize) + 1;
                    return true;
                }
                false
            }
            _ => {
                if let Ok(msg) = serde_json::from_value::<Message>(line_json.clone()) {
                    self.history.push(msg.clone());
                    messages_after_last_usage.push(msg);
                    true
                } else {
                    false
                }
            }
        }
    }
}

pub trait IntoMessages {
    fn into_messages(self) -> Vec<Message>;
}

impl IntoMessages for Message {
    fn into_messages(self) -> Vec<Message> {
        vec![self]
    }
}

impl IntoMessages for Vec<Message> {
    fn into_messages(self) -> Vec<Message> {
        self
    }
}

pub fn estimate_text_tokens(messages: &[Message]) -> usize {
    let total_chars: usize = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|p| match p {
            crate::wire::ContentPart::Text { text } => Some(text.len()),
            _ => None,
        })
        .sum();
    total_chars / 4
}

fn next_available_rotation(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let stem = path.file_stem()?.to_string_lossy();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    for i in 0..1000 {
        let name = if let Some(ref e) = ext {
            format!("{}.{:03}.{}", stem, i, e)
        } else {
            format!("{}.{:03}", stem, i)
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
