use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, TimeZone};
use extism::{CompiledPlugin, Manifest, Plugin, PluginBuilder, Wasm};
use futures_util::StreamExt;
use llm_provider::chat_provider::Part;
use llm_provider::message::{ContentPart, Message, Role};
use llm_provider::tooling::ToolReturnValue;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::config::{Config, LlmConfig};
use crate::onebot::types::Action;
use crate::onebot::ActionTx;

/// Minimum number of overlapping candidates needed to trigger a 3v3 match.
const PARTY_SIZE: usize = 6;

/// Retry delays after the initial notification.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(30), Duration::from_secs(30)];

/// Persistent state for one group.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PartyState {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Candidate {
    user_id: i64,
    #[serde(default)]
    nickname: String,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    joined_at: DateTime<FixedOffset>,
}

/// Public snapshot of the current party status, returned by `faf_party_status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartyStatus {
    pub count: usize,
    pub candidates: Vec<PartyCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartyCandidate {
    pub user_id: i64,
    pub nickname: String,
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
}

/// Serialized access to the per-group JSON state file.
///
/// All reads and writes go through a per-group `tokio::sync::Mutex` so that
/// concurrent messages (or a notification retry clearing the file) cannot
/// interleave and corrupt the state.
#[derive(Clone)]
pub struct PartyStateStore {
    data_dir: PathBuf,
    locks: Arc<Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>>,
}

impl PartyStateStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn state_path(&self, group_id: i64) -> PathBuf {
        self.data_dir.join(format!("faf-party-{group_id}.json"))
    }

    async fn lock(&self, group_id: i64) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks
                .entry(group_id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    async fn load(&self, group_id: i64) -> Result<PartyState> {
        let path = self.state_path(group_id);
        if !path.exists() {
            return Ok(PartyState::default());
        }
        let text = tokio::fs::read_to_string(&path)
            .await
            .context("failed to read party state file")?;
        serde_json::from_str(&text).context("invalid party state JSON")
    }

    async fn save(&self, group_id: i64, state: &PartyState) -> Result<()> {
        let path = self.state_path(group_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("failed to create party state directory")?;
        }
        let text =
            serde_json::to_string_pretty(state).context("failed to serialize party state")?;
        tokio::fs::write(&path, text)
            .await
            .context("failed to write party state file")?;
        Ok(())
    }

    pub async fn read_status(&self, group_id: i64) -> Result<PartyStatus> {
        let _guard = self.lock(group_id).await;
        let state = self.load(group_id).await?;
        Ok(PartyStatus {
            count: state.candidates.len(),
            candidates: state
                .candidates
                .into_iter()
                .map(|c| PartyCandidate {
                    user_id: c.user_id,
                    nickname: if c.nickname.is_empty() {
                        c.user_id.to_string()
                    } else {
                        c.nickname
                    },
                    start: c.start,
                    end: c.end,
                })
                .collect(),
        })
    }

    /// Remove expired candidates and persist if anything changed.
    pub async fn cleanup_expired(&self, group_id: i64, now: DateTime<FixedOffset>) -> Result<bool> {
        let _guard = self.lock(group_id).await;
        let mut state = self.load(group_id).await?;
        let before = state.candidates.len();
        state.candidates.retain(|c| c.end > now);
        let changed = state.candidates.len() != before;
        if changed {
            self.save(group_id, &state).await?;
        }
        Ok(changed)
    }

    pub async fn clear(&self, group_id: i64) -> Result<()> {
        let _guard = self.lock(group_id).await;
        self.save(group_id, &PartyState::default()).await
    }
}

/// Host-provided tool that lets the LLM query the current FAF party candidate list.
pub struct FafPartyStatusTool {
    store: PartyStateStore,
    group_id: i64,
}

impl FafPartyStatusTool {
    pub fn new(store: PartyStateStore, group_id: i64) -> Self {
        Self { store, group_id }
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct FafPartyStatusParams {
    /// No parameters needed; status is always for the current group.
    #[allow(dead_code)]
    _noop: Option<String>,
}

#[async_trait]
impl llm_provider::tooling::CallableTool2 for FafPartyStatusTool {
    type Params = FafPartyStatusParams;

    fn name(&self) -> &str {
        "faf_party_status"
    }

    fn description(&self) -> &str {
        "Get the current FAF party candidate list and overlap window for this group."
    }

    fn prompt_fragment(&self) -> Option<&str> {
        Some("When the user asks how many people are in the FAF party, who is playing, or when the party will happen, call faf_party_status to get the current candidates.")
    }

    async fn call_typed(&self, _params: FafPartyStatusParams) -> ToolReturnValue {
        match self.store.read_status(self.group_id).await {
            Ok(status) => {
                let text = serde_json::to_string(&status)
                    .unwrap_or_else(|_| "{\"count\":0,\"candidates\":[]}".to_string());
                ToolReturnValue::ok(text)
            }
            Err(e) => ToolReturnValue::error(format!("failed to read party status: {e}")),
        }
    }
}

/// Result returned by the `faf_party_parse_intent` plugin tool.
#[derive(Debug, Clone, Deserialize)]
struct ParseIntentResult {
    intent: String,
    time_expression: Option<String>,
    #[serde(default)]
    nickname: Option<String>,
}

/// Result returned by the `faf_party_parse_time` plugin tool.
#[derive(Debug, Clone, Deserialize)]
struct ParseTimeResult {
    unknown: bool,
    #[serde(default)]
    start: String,
    #[serde(default)]
    end: String,
}

/// Host-side service that owns FAF party scheduling state and timers.
#[derive(Clone)]
pub struct FafPartyHostService {
    store: PartyStateStore,
    action_tx: ActionTx,
    active_notifications: Arc<Mutex<HashMap<i64, JoinHandle<()>>>>,
    plugin: Option<CompiledPlugin>,
    llm_config: LlmConfig,
    /// Per-group nickname cache: group_id -> (user_id -> nickname).
    nicknames: Arc<Mutex<HashMap<i64, HashMap<i64, String>>>>,
}

impl FafPartyHostService {
    pub fn new(
        plugin_dir: PathBuf,
        data_dir: PathBuf,
        config: &Config,
        action_tx: ActionTx,
    ) -> Self {
        let plugin = Self::load_plugin(&plugin_dir);
        if plugin.is_none() {
            warn!(plugin_dir = %plugin_dir.display(), "faf-party plugin not loaded; party scheduling disabled");
        }
        Self {
            store: PartyStateStore::new(data_dir),
            action_tx,
            active_notifications: Arc::new(Mutex::new(HashMap::new())),
            plugin,
            llm_config: config.llm.clone(),
            nicknames: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn load_plugin(plugin_dir: &Path) -> Option<CompiledPlugin> {
        let path = plugin_dir.join("faf_party_plugin.wasm");
        let wasm_bytes = std::fs::read(&path)
            .map_err(|e| {
                warn!(path = %path.display(), error = %e, "failed to read faf-party plugin wasm");
            })
            .ok()?;

        let manifest = Manifest::new([Wasm::data(wasm_bytes)]);
        PluginBuilder::new(manifest)
            .with_wasi(true)
            .compile()
            .map_err(|e| {
                warn!(error = %e, "failed to compile faf-party plugin");
            })
            .ok()
    }

    pub fn state_store(&self) -> PartyStateStore {
        self.store.clone()
    }

    /// Process an addressed message for party scheduling intent.
    ///
    /// This is called before the normal Brain turn starts so that notifications
    /// can be sent immediately when enough players overlap.
    ///
    /// Returns `true` if the message was handled as a party registration/leave
    /// and the caller should skip the normal LLM turn.
    pub async fn process_message(
        &self,
        group_id: i64,
        user_id: i64,
        message: &str,
        now: DateTime<FixedOffset>,
        sender_nickname: Option<String>,
        at_targets: Vec<(i64, Option<String>)>,
    ) -> bool {
        match self
            .try_process_message(group_id, user_id, message, now, sender_nickname, at_targets)
            .await
        {
            Ok(handled) => handled,
            Err(e) => {
                error!(group_id, user_id, error = %e, "faf-party processing failed");
                false
            }
        }
    }

    async fn try_process_message(
        &self,
        group_id: i64,
        user_id: i64,
        message: &str,
        now: DateTime<FixedOffset>,
        sender_nickname: Option<String>,
        at_targets: Vec<(i64, Option<String>)>,
    ) -> Result<bool> {
        // Drop stale entries before acting on the current message.
        self.store.cleanup_expired(group_id, now).await?;

        // Update nickname cache with everyone visible in this message.
        self.remember_nickname(group_id, user_id, sender_nickname.clone())
            .await;
        for (target_id, target_nick) in &at_targets {
            self.remember_nickname(group_id, *target_id, target_nick.clone())
                .await;
        }

        let intent_parse = self.call_parse_intent(message).await?;
        info!(
            group_id,
            user_id,
            intent = %intent_parse.intent,
            time_expression = ?intent_parse.time_expression,
            nickname = ?intent_parse.nickname,
            "faf-party parsed intent"
        );

        let _guard = self.store.lock(group_id).await;
        let mut state = self.store.load(group_id).await?;

        match intent_parse.intent.as_str() {
            "leave" => {
                let target_ids: Vec<i64> = if at_targets.is_empty() {
                    vec![user_id]
                } else {
                    at_targets.iter().map(|(id, _)| *id).collect()
                };

                let mut removed = Vec::new();
                for id in target_ids {
                    if let Some(idx) = state.candidates.iter().position(|c| c.user_id == id) {
                        removed.push(state.candidates.remove(idx));
                    }
                }

                if !removed.is_empty() {
                    self.store.save(group_id, &state).await?;
                    info!(
                        group_id,
                        user_id,
                        count = removed.len(),
                        "removed faf-party candidates"
                    );
                    self.send_confirmation(group_id, None, &state, now).await?;
                }
                Ok(!removed.is_empty())
            }
            "join" => {
                // Resolve who is being registered.
                let targets: Vec<(i64, String)> = if at_targets.is_empty() {
                    let nick = intent_parse
                        .nickname
                        .clone()
                        .or(sender_nickname.clone())
                        .unwrap_or_else(|| user_id.to_string());
                    vec![(user_id, nick)]
                } else {
                    at_targets
                        .iter()
                        .map(|(id, fallback)| {
                            let nick = fallback
                                .clone()
                                .or_else(|| self.nickname_for(group_id, *id))
                                .unwrap_or_else(|| id.to_string());
                            (*id, nick)
                        })
                        .collect()
                };

                // Resolve the time window.
                let (start, end) = if let Some(expr) = intent_parse.time_expression.as_deref() {
                    let time_parse = self.parse_time_with_fallback(expr, now).await?;
                    let start = DateTime::parse_from_rfc3339(&time_parse.start)
                        .context("invalid availability start")?;
                    let end = DateTime::parse_from_rfc3339(&time_parse.end)
                        .context("invalid availability end")?;
                    (start, end)
                } else {
                    self.default_availability(now)
                };

                for (id, nick) in &targets {
                    state.candidates.retain(|c| c.user_id != *id);
                    state.candidates.push(Candidate {
                        user_id: *id,
                        nickname: nick.clone(),
                        start,
                        end,
                        joined_at: now,
                    });
                }

                self.store.save(group_id, &state).await?;
                info!(
                    group_id,
                    user_id,
                    targets = ?targets,
                    start = %start,
                    end = %end,
                    "added faf-party candidates"
                );

                self.send_confirmation(group_id, Some(&targets), &state, now)
                    .await?;

                if let Some(window) = find_overlap_window(&state.candidates) {
                    self.schedule_notifications(group_id, state.candidates.clone(), window)
                        .await;
                }

                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn remember_nickname(&self, group_id: i64, user_id: i64, nickname: Option<String>) {
        if let Some(nick) = nickname {
            let mut groups = self.nicknames.lock().await;
            groups
                .entry(group_id)
                .or_insert_with(HashMap::new)
                .insert(user_id, nick);
        }
    }

    fn nickname_for(&self, group_id: i64, user_id: i64) -> Option<String> {
        // Synchronous lookup into the cache. The cache is updated async, so a
        // very recent nickname may not be visible yet; callers fall back to the
        // user id in that case.
        if let Ok(groups) = self.nicknames.try_lock() {
            groups.get(&group_id).and_then(|m| m.get(&user_id)).cloned()
        } else {
            None
        }
    }

    /// Default availability window when the user wants to join but gives no time.
    ///
    /// Starts now and ends at 22:00 on the same day (or tomorrow if already past
    /// 22:00), in the group's local timezone.
    fn default_availability(
        &self,
        now: DateTime<FixedOffset>,
    ) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
        let tz = now.timezone();
        let today = now.date_naive();
        let end_time = chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap();
        let end = if let Some(dt) = tz.from_local_datetime(&today.and_time(end_time)).single() {
            if dt > now {
                dt
            } else {
                dt + chrono::Duration::days(1)
            }
        } else {
            now + chrono::Duration::hours(4)
        };
        (now, end)
    }

    /// Send a deterministic confirmation message with the current candidate list.
    async fn send_confirmation(
        &self,
        group_id: i64,
        added: Option<&[(i64, String)]>,
        state: &PartyState,
        now: DateTime<FixedOffset>,
    ) -> Result<()> {
        let mut lines = Vec::new();

        if let Some(targets) = added {
            for (_id, nick) in targets {
                lines.push(format!("已登记 {}。", nick));
            }
        } else {
            lines.push("已移除。".to_string());
        }

        lines.push(format!(
            "当前 party 名单（共 {} 人）：",
            state.candidates.len()
        ));
        if state.candidates.is_empty() {
            lines.push("  暂无".to_string());
        } else {
            for (idx, c) in state.candidates.iter().enumerate() {
                let display = if c.end > now {
                    format!("{} - {}", format_time(c.start), format_time(c.end))
                } else {
                    "已过期".to_string()
                };
                let nick = if c.nickname.is_empty() {
                    c.user_id.to_string()
                } else {
                    c.nickname.clone()
                };
                lines.push(format!("{}. {} — {}", idx + 1, nick, display));
            }
        }

        let text = lines.join("\n");
        let _ = self
            .action_tx
            .send(Action::send_group_msg(group_id, text, None));
        Ok(())
    }

    async fn parse_time_with_fallback(
        &self,
        expression: &str,
        now: DateTime<FixedOffset>,
    ) -> Result<ParseTimeResult> {
        // First try the rule-based plugin parser.
        let rule_result = self.call_parse_time(expression, now).await?;
        if !rule_result.unknown {
            return Ok(rule_result);
        }

        // Rule-based failed; ask the LLM to parse it.
        info!(
            expression,
            "rule-based time parse failed; falling back to LLM"
        );
        self.llm_parse_time(expression, now).await
    }

    async fn call_parse_intent(&self, message: &str) -> Result<ParseIntentResult> {
        self.call_plugin(
            "faf_party_parse_intent",
            serde_json::json!({"message": message}),
        )
        .await
    }

    async fn call_parse_time(
        &self,
        expression: &str,
        now: DateTime<FixedOffset>,
    ) -> Result<ParseTimeResult> {
        self.call_plugin(
            "faf_party_parse_time",
            serde_json::json!({
                "expression": expression,
                "now": now.to_rfc3339(),
            }),
        )
        .await
    }

    async fn call_plugin<T: serde::de::DeserializeOwned>(
        &self,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<T> {
        let compiled = self
            .plugin
            .as_ref()
            .context("faf-party plugin not loaded")?;

        let input = serde_json::json!({
            "tool": tool,
            "arguments": arguments,
        })
        .to_string();

        let output_str = tokio::task::spawn_blocking({
            let compiled = compiled.clone();
            let input = input.clone();
            move || {
                let mut plugin = Plugin::new_from_compiled(&compiled)
                    .map_err(|e| anyhow::anyhow!("failed to instantiate plugin: {e}"))?;
                plugin
                    .call::<&str, &str>("execute", &input)
                    .map_err(|e| anyhow::anyhow!("plugin execute error: {e}"))
                    .map(|s| s.to_string())
            }
        })
        .await
        .context("plugin task panicked")?
        .context("plugin execution failed")?;

        serde_json::from_str(&output_str).context("failed to parse plugin output")
    }

    async fn llm_parse_time(
        &self,
        expression: &str,
        now: DateTime<FixedOffset>,
    ) -> Result<ParseTimeResult> {
        let brain_config = agent_core::BrainConfig {
            model: self.llm_config.model.clone(),
            base_url: self.llm_config.api_url.clone(),
            system_prompt: self.llm_config.system_prompt.clone(),
            provider_type: self.llm_config.provider.clone(),
            ..Default::default()
        };
        let provider = brain_config
            .build_provider()
            .await
            .context("failed to create LLM provider for time parsing fallback")?;

        let system = "You are a precise time parser. Given a Chinese time expression and the current time, return a JSON object with 'start' and 'end' keys in RFC3339 format. Use +08:00 timezone. If the expression does not contain an end time, default to 22:00 on the same day. If you cannot parse the expression, return JSON with only \"unknown\": true. Return ONLY the JSON object, no markdown, no explanation.";
        let user = format!(
            "Expression: \"{}\"\nNow: {}\nReturn JSON:",
            expression,
            now.to_rfc3339()
        );

        let messages = vec![
            Message {
                role: Role::System,
                name: None,
                content: vec![ContentPart::Text {
                    text: system.to_string(),
                }],
                tool_calls: None,
                tool_call_id: None,
                partial: None,
            },
            Message {
                role: Role::User,
                name: None,
                content: vec![ContentPart::Text { text: user }],
                tool_calls: None,
                tool_call_id: None,
                partial: None,
            },
        ];

        let response = provider
            .generate(system, &[], &messages)
            .await
            .context("LLM fallback generate failed")?;

        let mut text = String::new();
        let mut stream = response.stream;
        while let Some(part) = stream.next().await {
            if let Part::Content(ContentPart::Text { text: t }) = part {
                text.push_str(&t);
            }
        }

        let cleaned = text.trim();
        // Remove possible markdown code fences.
        let cleaned = cleaned
            .strip_prefix("```json")
            .or_else(|| cleaned.strip_prefix("```"))
            .unwrap_or(cleaned)
            .trim();
        let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

        let result: ParseTimeResult =
            serde_json::from_str(cleaned).context("failed to parse LLM fallback JSON")?;
        if result.unknown || result.start.is_empty() || result.end.is_empty() {
            anyhow::bail!(
                "LLM fallback could not parse time expression: {}",
                expression
            );
        }
        Ok(result)
    }

    async fn schedule_notifications(
        &self,
        group_id: i64,
        candidates: Vec<Candidate>,
        window: (DateTime<FixedOffset>, DateTime<FixedOffset>),
    ) {
        // Cancel any pending retry loop for this group.
        {
            let mut active = self.active_notifications.lock().await;
            if let Some(old) = active.remove(&group_id) {
                old.abort();
            }
        }

        let action_tx = self.action_tx.clone();
        let store = self.store.clone();
        let active_notifications = self.active_notifications.clone();

        let handle = tokio::spawn(async move {
            let user_ids: Vec<i64> = candidates.iter().map(|c| c.user_id).collect();
            let count = candidates.len();
            let (win_start, win_end) = window;

            for attempt in 0..=RETRY_DELAYS.len() {
                if attempt > 0 {
                    tokio::time::sleep(RETRY_DELAYS[attempt - 1]).await;
                }

                let text = format_party_notification(count, win_start, win_end, attempt);
                let action = if attempt == 0 {
                    Action::send_group_msg_with_mentions(group_id, &user_ids, &text, None)
                } else {
                    Action::send_group_msg(group_id, text, None)
                };

                let _ = action_tx.send(action);
                info!(group_id, attempt, "sent faf-party notification");
            }

            // After the final retry, clear the candidate list.
            if let Err(e) = store.clear(group_id).await {
                warn!(group_id, error = %e, "failed to clear party state after retries");
            }

            let mut active = active_notifications.lock().await;
            active.remove(&group_id);
        });

        let mut active = self.active_notifications.lock().await;
        active.insert(group_id, handle);
    }
}

fn format_party_notification(
    count: usize,
    win_start: DateTime<FixedOffset>,
    win_end: DateTime<FixedOffset>,
    attempt: usize,
) -> String {
    let window_desc = format!("{} - {}", format_time(win_start), format_time(win_end));
    match attempt {
        0 => format!("🎮 已有 {count} 位玩家可以组队开黑！重叠时间：{window_desc}。速来！"),
        1 => format!("🎮 还有 {count} 位玩家在等，重叠时间：{window_desc}。还有人吗？"),
        _ => format!("🎮 最后召集：{count} 位玩家，重叠时间：{window_desc}。"),
    }
}

fn format_time(dt: DateTime<FixedOffset>) -> String {
    dt.format("%H:%M").to_string()
}

/// Find a time window where at least `PARTY_SIZE` candidates overlap.
fn find_overlap_window(
    candidates: &[Candidate],
) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    if candidates.len() < PARTY_SIZE {
        return None;
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Start,
        End,
    }

    let mut events: Vec<(DateTime<FixedOffset>, Kind)> = Vec::new();
    for c in candidates {
        events.push((c.start, Kind::Start));
        events.push((c.end, Kind::End));
    }
    // Sort by time; for equal times, Start comes before End so overlapping at
    // the boundary still counts.
    events.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| match (a.1, b.1) {
            (Kind::Start, Kind::End) => std::cmp::Ordering::Less,
            (Kind::End, Kind::Start) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        })
    });

    let mut active = 0;
    let mut current_start: Option<DateTime<FixedOffset>> = None;
    let mut best: Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> = None;

    for (time, kind) in events {
        match kind {
            Kind::Start => {
                active += 1;
                if active >= PARTY_SIZE && current_start.is_none() {
                    current_start = Some(time);
                }
            }
            Kind::End => {
                if active >= PARTY_SIZE {
                    if let Some(start) = current_start {
                        best = Some((start, time));
                    }
                }
                active = active.saturating_sub(1);
                if active < PARTY_SIZE {
                    current_start = None;
                }
            }
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32, minute: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 6, 18, hour, minute, 0)
            .unwrap()
    }

    fn candidate(
        user_id: i64,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Candidate {
        Candidate {
            user_id,
            nickname: format!("user{user_id}"),
            start,
            end,
            joined_at: start,
        }
    }

    #[test]
    fn no_overlap_with_five_candidates() {
        let candidates = vec![
            candidate(1, dt(19, 0), dt(22, 0)),
            candidate(2, dt(19, 0), dt(22, 0)),
            candidate(3, dt(19, 0), dt(22, 0)),
            candidate(4, dt(19, 0), dt(22, 0)),
            candidate(5, dt(19, 0), dt(22, 0)),
        ];
        assert!(find_overlap_window(&candidates).is_none());
    }

    #[test]
    fn six_candidates_overlap() {
        let candidates = vec![
            candidate(1, dt(19, 0), dt(22, 0)),
            candidate(2, dt(19, 0), dt(22, 0)),
            candidate(3, dt(19, 0), dt(22, 0)),
            candidate(4, dt(19, 0), dt(22, 0)),
            candidate(5, dt(19, 0), dt(22, 0)),
            candidate(6, dt(19, 0), dt(22, 0)),
        ];
        let (start, end) = find_overlap_window(&candidates).unwrap();
        assert_eq!(start, dt(19, 0));
        assert_eq!(end, dt(22, 0));
    }

    #[test]
    fn partial_overlap_finds_common_window() {
        let candidates = vec![
            candidate(1, dt(19, 0), dt(22, 0)),
            candidate(2, dt(19, 0), dt(22, 0)),
            candidate(3, dt(19, 0), dt(22, 0)),
            candidate(4, dt(20, 0), dt(22, 0)),
            candidate(5, dt(20, 0), dt(22, 0)),
            candidate(6, dt(20, 0), dt(21, 0)),
        ];
        let (start, end) = find_overlap_window(&candidates).unwrap();
        assert_eq!(start, dt(20, 0));
        assert_eq!(end, dt(21, 0));
    }
}
