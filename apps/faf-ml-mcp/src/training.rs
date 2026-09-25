//! Training-run manager: owns the `/ws/training` WebSocket internally so
//! MCP callers only see plain tool calls (`training_start` → handle,
//! `training_status`, `training_command`).
//!
//! Run handles are server-assigned run ids: `TrainingCommand::Start` is
//! acknowledged with `TrainingEvent::Started { id }`, and that id scopes
//! every later command and event on the socket.

use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, Context};
use faf_ml_core::{
    TrainingCommand, TrainingConfig, TrainingEvent, TrainingMetricsPoint, TrainingStatus,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

/// Live state of one training run, updated by the socket reader task.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RunState {
    /// Last status reported by the server (`None` until the first arrives).
    pub status: Option<TrainingStatus>,
    /// Free-form note: protocol/socket errors, cleared-by-reset notices.
    pub detail: String,
    /// Metrics points received so far.
    pub points: usize,
    /// The most recent metrics point (progress + current losses).
    pub latest: Option<TrainingMetricsPoint>,
}

struct RunHandle {
    state: Arc<Mutex<RunState>>,
    cmd_tx: mpsc::UnboundedSender<TrainingCommand>,
}

/// Registry of training runs started through this MCP server (in-memory;
/// runs do not survive a server restart). (Client-side view of the server's
/// training manager — hence `TrainingRuns`, not `TrainingManager`.)
#[derive(Clone, Default)]
pub struct TrainingRuns {
    runs: Arc<Mutex<HashMap<Uuid, RunHandle>>>,
    last_started: Arc<Mutex<Option<Uuid>>>,
}

impl TrainingRuns {
    /// Open `/ws/training`, send `Start { config, speed }`, wait for the
    /// `Started { id }` ack, and spawn the reader task. Returns the
    /// server-assigned run id (the handle for the other tools).
    pub async fn start(
        &self,
        api_base: &str,
        config: TrainingConfig,
        speed: f64,
    ) -> anyhow::Result<Uuid> {
        let ws_base = match api_base.strip_prefix("https") {
            Some(rest) => format!("wss{rest}"),
            None => api_base.replacen("http", "ws", 1),
        };
        let url = format!("{ws_base}/ws/training");
        let (socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let (mut sink, mut stream) = socket.split();

        let start = serde_json::to_string(&TrainingCommand::Start { config, speed })?;
        sink.send(Message::Text(start.into())).await?;

        // The server answers Start with the Started ack carrying the run id.
        let run_id = loop {
            match stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<TrainingEvent>(&text) {
                        Ok(TrainingEvent::Started { id }) => break id,
                        Ok(TrainingEvent::Error { message }) => {
                            return Err(anyhow!("server rejected the run: {message}"));
                        }
                        Ok(_) => {}
                        Err(err) => return Err(anyhow!("invalid server frame: {err}")),
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    return Err(anyhow!("server closed the socket before confirming the run"));
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(anyhow!("socket error: {err}")),
            }
        };

        let state = Arc::new(Mutex::new(RunState::default()));
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<TrainingCommand>();

        let task_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    frame = stream.next() => {
                        match frame {
                            Some(Ok(Message::Text(text))) => {
                                match serde_json::from_str::<TrainingEvent>(&text) {
                                    // Run-scoped events carry the run id; ignore
                                    // foreign ids (defensive — the server
                                    // already filters per socket).
                                    Ok(TrainingEvent::Metrics { id, point }) if id == run_id => {
                                        let mut s = task_state.lock().await;
                                        s.points += 1;
                                        s.latest = Some(point);
                                    }
                                    Ok(TrainingEvent::Status { id, status }) if id == run_id => {
                                        task_state.lock().await.status = Some(status);
                                    }
                                    Ok(TrainingEvent::Cleared { id }) if id == run_id => {
                                        let mut s = task_state.lock().await;
                                        s.detail =
                                            "run cleared — record wiped, charts cleared".into();
                                        s.points = 0;
                                        s.latest = None;
                                    }
                                    Ok(TrainingEvent::Finished { id }) if id == run_id => break,
                                    // Error is not run-scoped; the Started ack was
                                    // already consumed by `start`.
                                    Ok(TrainingEvent::Error { message }) => {
                                        task_state.lock().await.detail =
                                            format!("error: {message}");
                                    }
                                    Ok(_) => {}
                                    Err(err) => {
                                        task_state.lock().await.detail =
                                            format!("parse error: {err}");
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(err)) => {
                                task_state.lock().await.detail = format!("socket error: {err}");
                                break;
                            }
                        }
                    }
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else { break };
                        let text = serde_json::to_string(&cmd).unwrap_or_default();
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        self.runs
            .lock()
            .await
            .insert(run_id, RunHandle { state, cmd_tx });
        *self.last_started.lock().await = Some(run_id);
        Ok(run_id)
    }

    async fn resolve(&self, handle: Option<&str>) -> anyhow::Result<Uuid> {
        match handle {
            Some(h) => Uuid::parse_str(h).with_context(|| format!("invalid run handle {h:?}")),
            None => self
                .last_started
                .lock()
                .await
                .ok_or_else(|| anyhow!("no training runs yet in this MCP server session")),
        }
    }

    /// Snapshot of one run's state (most recent when `handle` is omitted).
    pub async fn status(&self, handle: Option<&str>) -> anyhow::Result<RunState> {
        let id = self.resolve(handle).await?;
        let state = {
            let runs = self.runs.lock().await;
            let run = runs
                .get(&id)
                .ok_or_else(|| anyhow!("unknown training run {id}"))?;
            run.state.clone()
        };
        let snapshot = state.lock().await.clone();
        Ok(snapshot)
    }

    /// Forward a command to a running job. The command must carry the same
    /// run id as the handle.
    pub async fn command(&self, handle: &str, cmd: TrainingCommand) -> anyhow::Result<()> {
        let id =
            Uuid::parse_str(handle).with_context(|| format!("invalid run handle {handle:?}"))?;
        if cmd.run_id() != Some(id) {
            return Err(anyhow!("command run id does not match handle {handle}"));
        }
        let runs = self.runs.lock().await;
        let run = runs
            .get(&id)
            .ok_or_else(|| anyhow!("unknown training run {id}"))?;
        run.cmd_tx
            .send(cmd)
            .map_err(|_| anyhow!("training run {id} is no longer connected"))
    }
}
