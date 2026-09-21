//! `TrainManager`: training as a long-running async service.
//!
//! One tokio task (the actor) owns a registry of training runs
//! (`HashMap<Uuid, RunState>`) and is its sole mutator. Callers hold a
//! [`TrainManagerHandle`] and talk to it over a mailbox of [`TrainCmd`]s
//! (every command carries a oneshot reply); viewers subscribe to the
//! [`ManagerEvent`] broadcast. There is one broadcast channel for all runs —
//! every event carries the run id and viewers filter by it. The burn training
//! loop is sync, so it stays on a `std::thread`: control flows in via a
//! `watch` channel (read at batch boundaries), events flow out over an
//! unbounded channel.
//!
//! Late events from a winding-down (reset) run are ignored via the **run
//! id**: Reset removes the run from the registry, so a thread message whose
//! id is no longer in `runs` is dropped.
//!
//! ```text
//! web UI / MCP / REST  ──TrainCmd over mpsc (oneshot replies)──▶ TrainManager
//!                                                                   │  watch<ControlState> in
//!                                                              std::thread running train()
//!                                                                   │  (Uuid, ThreadMsg) out
//! ```

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use faf_ml_core::TrainingCommand;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use uuid::Uuid;

use crate::train::{train, ControlState, TrainAction, TrainEvent, TrainExit, TrainParams};
use crate::{AdB, CpuAdB};

/// Phase of an active run. `Pausing`/`Stopping` mean the command was
/// accepted but the training thread has not reached the batch boundary yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Pausing,
    Paused,
    Stopping,
}

/// Terminal outcome of a run.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Completed {
        run_dir: PathBuf,
        duration_secs: u64,
    },
    /// Ended via Stop OR Reset — the checkpoint was saved either way.
    Stopped {
        run_dir: PathBuf,
        duration_secs: u64,
    },
    Failed {
        error: String,
    },
}

/// Lifecycle of one training run. Absence from the manager's registry means
/// idle — there is no `Idle` variant.
#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Active {
        phase: Phase,
        config: TrainParams,
        started_at: DateTime<Utc>,
    },
    Ended {
        outcome: Outcome,
        config: TrainParams,
        started_at: DateTime<Utc>,
    },
}

/// Event broadcast to all viewers. Every variant carries the run id; viewers
/// filter by the run they started or attached to.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagerEvent {
    /// A training metrics event (Batch / EpochEnd).
    Train { id: Uuid, event: TrainEvent },
    /// Instant control acknowledgment (Pausing/Stopping) or settled phase
    /// (Running/Paused).
    PhaseChanged { id: Uuid, phase: Phase },
    /// Terminal outcome of the run.
    Ended { id: Uuid, outcome: Outcome },
    /// The run record was wiped — viewers clear their charts.
    Reset { id: Uuid },
}

/// Atomic snapshot for late/reattaching viewers: run status, replay buffer,
/// and a live subscription, all taken under one actor turn so no event can
/// interleave between them.
pub struct AttachDump {
    pub id: Uuid,
    pub status: RunStatus,
    /// Buffered Batch/EpochEnd events (replay for new viewers). Batch points
    /// are capped at [`MAX_REPLAY_BATCH_EVENTS`] (oldest dropped); epoch
    /// summaries are kept for the whole run.
    pub replay: Vec<TrainEvent>,
    pub events: broadcast::Receiver<ManagerEvent>,
}

/// Per-batch points retained in the replay buffer. Epoch summaries never
/// count against the cap — a long run's memory stays bounded while the
/// epoch-level curve remains complete.
const MAX_REPLAY_BATCH_EVENTS: usize = 5000;

/// Point-in-time snapshot for `GET /api/training/status`.
pub struct Snapshot {
    pub id: Uuid,
    pub status: RunStatus,
    pub replay: Vec<TrainEvent>,
}

/// Max concurrent active runs. This is the GPU resource policy — the
/// training buffer is sized for batch ≤ 4, so only one run can train at a
/// time — not an architectural limit of the run registry.
const MAX_RUNS: usize = 1;

/// Message from a training thread to the manager (tagged with the run id so
/// a winding-down run's late messages are dropped after Reset).
enum ThreadMsg {
    Event(TrainEvent),
    /// String (not anyhow::Error) so the message stays `Send` + simple.
    Exited(Result<TrainExit, String>),
}

/// Mailbox message to the manager actor (all carry a oneshot reply).
enum TrainCmd {
    Start {
        params: TrainParams,
        speed: f64,
        reply: oneshot::Sender<Result<Uuid, String>>,
    },
    Command {
        cmd: TrainingCommand,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Attach {
        id: Uuid,
        reply: oneshot::Sender<Result<AttachDump, String>>,
    },
    Snapshot {
        id: Option<Uuid>,
        reply: oneshot::Sender<Result<Snapshot, String>>,
    },
}

/// Spawns a training backend for one run. The default spawns the real burn
/// thread; tests inject a fake driving the same channels.
type TrainerFactory = Arc<
    dyn Fn(TrainParams, watch::Receiver<ControlState>, mpsc::UnboundedSender<(Uuid, ThreadMsg)>, Uuid)
        + Send
        + Sync,
>;

/// Cloneable handle to the manager actor.
#[derive(Clone)]
pub struct TrainManagerHandle {
    cmd_tx: mpsc::Sender<TrainCmd>,
}

impl TrainManagerHandle {
    /// Spawn the manager actor on the caller's tokio runtime.
    pub fn spawn() -> Self {
        Self::spawn_with(Arc::new(spawn_train_thread))
    }

    fn spawn_with(trainer: TrainerFactory) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (thread_tx, thread_rx) = mpsc::unbounded_channel();
        let (events_tx, _) = broadcast::channel(1024);
        let manager = TrainManager {
            runs: HashMap::new(),
            events_tx,
            trainer,
            cmd_rx,
            thread_tx,
            thread_rx,
        };
        tokio::spawn(manager.run());
        Self { cmd_tx }
    }

    /// Start a run; the manager assigns and returns its run id. Busy/dataset
    /// validation failures come back in the reply.
    pub async fn start(&self, params: TrainParams, speed: f64) -> Result<Uuid, String> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(TrainCmd::Start {
                params,
                speed,
                reply,
            })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Send a runtime command (Pause/Resume/Stop/Reset/SetSpeed) addressed
    /// to the run id inside the command.
    pub async fn command(&self, cmd: TrainingCommand) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(TrainCmd::Command { cmd, reply })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Atomic replay + live subscription for a (re)attaching viewer of run
    /// `id`.
    pub async fn attach(&self, id: Uuid) -> Result<AttachDump, String> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(TrainCmd::Attach { id, reply })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Point-in-time status (for the REST status endpoint). `None` resolves
    /// to the active run if one exists, else the most recently ended run.
    pub async fn snapshot(&self, id: Option<Uuid>) -> Result<Snapshot, String> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(TrainCmd::Snapshot { id, reply })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }
}

fn gone() -> String {
    "training manager is gone".to_string()
}

fn unknown_run(id: Uuid) -> String {
    format!("unknown training run {id}")
}

/// Spawn the real training thread: `train()` on one thread, plus a tiny
/// forwarder thread that tags events with the run id. The forwarder is
/// joined before `Exited` is sent so the exit can never overtake metrics.
fn spawn_train_thread(
    params: TrainParams,
    control: watch::Receiver<ControlState>,
    events: mpsc::UnboundedSender<(Uuid, ThreadMsg)>,
    id: Uuid,
) {
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<TrainEvent>();
    let fwd_tx = events.clone();
    let forwarder = std::thread::spawn(move || {
        while let Some(event) = ev_rx.blocking_recv() {
            if fwd_tx.send((id, ThreadMsg::Event(event))).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        let result = if params.config.cpu {
            train::<CpuAdB>(&params, ev_tx, control)
        } else {
            train::<AdB>(&params, ev_tx, control)
        };
        let exit = result.map_err(|e| format!("{e:#}"));
        // `train` dropped `ev_tx` on return, so the forwarder has flushed
        // every event once it exits.
        let _ = forwarder.join();
        let _ = events.send((id, ThreadMsg::Exited(exit)));
    });
}

/// State of one registered run.
struct RunState {
    status: RunStatus,
    /// Buffered Batch/EpochEnd events (replayed to new viewers; survives
    /// into `Ended` so late attachers still see the last run). Batch points
    /// are capped (see [`MAX_REPLAY_BATCH_EVENTS`]); epoch ends are kept.
    replay: VecDeque<TrainEvent>,
    /// Number of `Batch` events currently in `replay` (epoch ends excluded).
    replay_batches: usize,
    /// Control channel of the training thread (`None` once the run ended).
    control_tx: Option<watch::Sender<ControlState>>,
}

impl RunState {
    fn active_phase(&self) -> Result<Phase, String> {
        match &self.status {
            RunStatus::Active { phase, .. } => Ok(*phase),
            _ => Err("no active run".to_string()),
        }
    }

    fn set_action(&self, action: TrainAction) {
        if let Some(tx) = &self.control_tx {
            tx.send_modify(|c| c.action = action);
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        if let RunStatus::Active { phase: p, .. } = &mut self.status {
            *p = phase;
        }
    }

    /// Buffer an event for late attachers. Epoch summaries are kept for the
    /// whole run; batch points beyond [`MAX_REPLAY_BATCH_EVENTS`] drop the
    /// oldest first, so a long run can't grow memory without bound.
    fn push_replay(&mut self, event: TrainEvent) {
        let is_batch = matches!(event, TrainEvent::Batch { .. });
        self.replay.push_back(event);
        if !is_batch {
            return;
        }
        self.replay_batches += 1;
        if self.replay_batches > MAX_REPLAY_BATCH_EVENTS {
            let oldest_batch = self
                .replay
                .iter()
                .position(|e| matches!(e, TrainEvent::Batch { .. }));
            if let Some(pos) = oldest_batch {
                self.replay.remove(pos);
                self.replay_batches -= 1;
            }
        }
    }
}

/// The manager actor: single `select!` loop, sole mutator of the run
/// registry.
struct TrainManager {
    runs: HashMap<Uuid, RunState>,
    events_tx: broadcast::Sender<ManagerEvent>,
    trainer: TrainerFactory,
    cmd_rx: mpsc::Receiver<TrainCmd>,
    thread_tx: mpsc::UnboundedSender<(Uuid, ThreadMsg)>,
    thread_rx: mpsc::UnboundedReceiver<(Uuid, ThreadMsg)>,
}

impl TrainManager {
    async fn run(mut self) {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_cmd(cmd),
                        None => break,
                    }
                }
                msg = self.thread_rx.recv() => {
                    if let Some((id, msg)) = msg {
                        self.handle_thread_msg(id, msg);
                    }
                }
            }
        }
    }

    fn handle_cmd(&mut self, cmd: TrainCmd) {
        match cmd {
            TrainCmd::Start {
                params,
                speed,
                reply,
            } => {
                let _ = reply.send(self.start(params, speed));
            }
            TrainCmd::Command { cmd, reply } => {
                let _ = reply.send(self.command(cmd));
            }
            TrainCmd::Attach { id, reply } => {
                let _ = reply.send(self.attach(id));
            }
            TrainCmd::Snapshot { id, reply } => {
                let _ = reply.send(self.snapshot(id));
            }
        }
    }

    fn start(&mut self, params: TrainParams, speed: f64) -> Result<Uuid, String> {
        let active = self
            .runs
            .values()
            .filter(|run| matches!(run.status, RunStatus::Active { .. }))
            .count();
        if active >= MAX_RUNS {
            return Err("a training run is already active".to_string());
        }
        validate_dataset(&params)?;
        let id = Uuid::new_v4();
        let (control_tx, control_rx) = watch::channel(ControlState {
            action: TrainAction::Continue,
            batches_per_sec: speed,
        });
        (self.trainer)(params.clone(), control_rx, self.thread_tx.clone(), id);
        self.runs.insert(
            id,
            RunState {
                status: RunStatus::Active {
                    phase: Phase::Running,
                    config: params,
                    started_at: Utc::now(),
                },
                replay: VecDeque::new(),
                replay_batches: 0,
                control_tx: Some(control_tx),
            },
        );
        self.broadcast(ManagerEvent::PhaseChanged {
            id,
            phase: Phase::Running,
        });
        Ok(id)
    }

    fn command(&mut self, cmd: TrainingCommand) -> Result<(), String> {
        match cmd {
            TrainingCommand::Start { .. } | TrainingCommand::Attach { .. } => {
                Err("not a runtime command: use start()/attach()".to_string())
            }
            TrainingCommand::Reset { id } => {
                let run = self.runs.get(&id).ok_or_else(|| unknown_run(id))?;
                // Checkpoint still saved: the thread sees Abort and exits
                // through the normal finish path; its late messages are
                // dropped because the id is gone from the registry.
                if let Some(tx) = &run.control_tx {
                    tx.send_modify(|c| c.action = TrainAction::Abort);
                }
                self.runs.remove(&id);
                self.broadcast(ManagerEvent::Reset { id });
                Ok(())
            }
            TrainingCommand::Pause { id } => {
                let run = self.run_mut(id)?;
                if matches!(run.active_phase()?, Phase::Running) {
                    run.set_action(TrainAction::Pause);
                    run.set_phase(Phase::Pausing);
                    self.broadcast(ManagerEvent::PhaseChanged {
                        id,
                        phase: Phase::Pausing,
                    });
                }
                Ok(())
            }
            TrainingCommand::Resume { id } => {
                let run = self.run_mut(id)?;
                if matches!(run.active_phase()?, Phase::Pausing | Phase::Paused) {
                    run.set_action(TrainAction::Continue);
                    run.set_phase(Phase::Running);
                    self.broadcast(ManagerEvent::PhaseChanged {
                        id,
                        phase: Phase::Running,
                    });
                }
                Ok(())
            }
            TrainingCommand::Stop { id } => {
                let run = self.run_mut(id)?;
                if !matches!(run.active_phase()?, Phase::Stopping) {
                    run.set_action(TrainAction::Abort);
                    run.set_phase(Phase::Stopping);
                    self.broadcast(ManagerEvent::PhaseChanged {
                        id,
                        phase: Phase::Stopping,
                    });
                }
                Ok(())
            }
            TrainingCommand::SetSpeed {
                id,
                batches_per_sec,
            } => {
                let run = self.run_mut(id)?;
                run.active_phase()?;
                if let Some(tx) = &run.control_tx {
                    tx.send_modify(|c| c.batches_per_sec = batches_per_sec);
                }
                Ok(())
            }
        }
    }

    fn attach(&self, id: Uuid) -> Result<AttachDump, String> {
        let run = self.runs.get(&id).ok_or_else(|| unknown_run(id))?;
        Ok(AttachDump {
            id,
            status: run.status.clone(),
            replay: run.replay.iter().cloned().collect(),
            events: self.events_tx.subscribe(),
        })
    }

    fn snapshot(&self, id: Option<Uuid>) -> Result<Snapshot, String> {
        let (id, run) = match id {
            Some(id) => (id, self.runs.get(&id).ok_or_else(|| unknown_run(id))?),
            None => self.latest_run().ok_or("no training run".to_string())?,
        };
        Ok(Snapshot {
            id,
            status: run.status.clone(),
            replay: run.replay.iter().cloned().collect(),
        })
    }

    /// The run a bare `snapshot(None)` refers to: the active run if one
    /// exists, else the most recently started ended run.
    fn latest_run(&self) -> Option<(Uuid, &RunState)> {
        self.runs.iter().max_by_key(|(id, run)| {
            let (active, started_at) = match &run.status {
                RunStatus::Active { started_at, .. } => (1, started_at),
                RunStatus::Ended { started_at, .. } => (0, started_at),
            };
            (active, started_at, *id)
        })
        .map(|(id, run)| (*id, run))
    }

    fn run_mut(&mut self, id: Uuid) -> Result<&mut RunState, String> {
        self.runs.get_mut(&id).ok_or_else(|| unknown_run(id))
    }

    fn handle_thread_msg(&mut self, id: Uuid, msg: ThreadMsg) {
        if !self.runs.contains_key(&id) {
            return; // winding-down run after Reset — its id is gone
        }
        match msg {
            ThreadMsg::Event(event) => {
                let run = self.runs.get_mut(&id).expect("checked above");
                if matches!(event, TrainEvent::Paused) {
                    // A stale Paused arriving while Running (thread entered
                    // the hold just as Resume landed) is ignored.
                    if matches!(
                        &run.status,
                        RunStatus::Active {
                            phase: Phase::Pausing,
                            ..
                        }
                    ) {
                        run.set_phase(Phase::Paused);
                        self.broadcast(ManagerEvent::PhaseChanged {
                            id,
                            phase: Phase::Paused,
                        });
                    }
                    return;
                }
                run.push_replay(event.clone());
                self.broadcast(ManagerEvent::Train { id, event });
            }
            ThreadMsg::Exited(result) => {
                let outcome = match result {
                    Ok(TrainExit::Completed {
                        run_dir,
                        duration_secs,
                    }) => Outcome::Completed {
                        run_dir,
                        duration_secs,
                    },
                    Ok(TrainExit::Aborted {
                        run_dir,
                        duration_secs,
                    }) => Outcome::Stopped {
                        run_dir,
                        duration_secs,
                    },
                    Err(error) => Outcome::Failed { error },
                };
                let mut run = self.runs.remove(&id).expect("checked above");
                run.control_tx = None;
                if let RunStatus::Active {
                    config, started_at, ..
                } = run.status
                {
                    run.status = RunStatus::Ended {
                        outcome: outcome.clone(),
                        config,
                        started_at,
                    };
                    self.broadcast(ManagerEvent::Ended { id, outcome });
                }
                self.runs.insert(id, run);
            }
        }
    }

    fn broadcast(&self, event: ManagerEvent) {
        let _ = self.events_tx.send(event);
    }
}

/// Snapshot pre-flight check, run synchronously in `start` so a bad snapshot
/// fails the Start request itself instead of surfacing as a `Failed` run
/// seconds later. Checks the name, and that the manifest exists, parses, and
/// has enough samples (`TrainParams` carries the store paths alongside the
/// embedded config, so the manager stays path-agnostic; snapshots live in
/// `datasets/` under the store root by convention). Image/label integrity is
/// still verified by `DetectDataset::load_snapshot` on the training thread.
fn validate_dataset(params: &TrainParams) -> Result<(), String> {
    let dataset = &params.config.dataset;
    let name = dataset.trim();
    if name.is_empty() {
        return Err(
            "no dataset snapshot selected — create one on the Datasets page first".to_string(),
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "invalid dataset name {name:?}: only [A-Za-z0-9._-] allowed"
        ));
    }
    let snapshot = params
        .data
        .join("datasets")
        .join(format!("{dataset}.json"));
    if !snapshot.is_file() {
        return Err(format!(
            "snapshot {dataset:?} not found — create one on the Datasets page first"
        ));
    }
    let raw = std::fs::read_to_string(&snapshot)
        .map_err(|e| format!("reading {}: {e}", snapshot.display()))?;
    let manifest: faf_ml_core::DatasetManifest =
        serde_json::from_str(&raw).map_err(|e| format!("parsing {}: {e}", snapshot.display()))?;
    if manifest.entries.len() < 2 {
        return Err(format!(
            "snapshot {dataset:?} has {} sample(s) — need at least 2 to train",
            manifest.entries.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use faf_ml_core::TrainingConfig;
    use std::time::Duration;

    /// A store dir with a `datasets/test.json` snapshot in it (valid
    /// manifest, 2 entries — passes the start pre-flight).
    fn test_params() -> (PathBuf, TrainParams) {
        let dir = std::env::temp_dir().join(format!("train-manager-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("datasets")).unwrap();
        let manifest = serde_json::json!({
            "name": "test",
            "created_at": "2026-01-01T00:00:00Z",
            "entries": [
                {"image_id": uuid::Uuid::new_v4(), "labels": []},
                {"image_id": uuid::Uuid::new_v4(), "labels": []},
            ],
        });
        std::fs::write(dir.join("datasets/test.json"), manifest.to_string()).unwrap();
        let params = TrainParams {
            config: TrainingConfig {
                dataset: "test".to_string(),
                ..Default::default()
            },
            data: dir.clone(),
            ..Default::default()
        };
        (dir, params)
    }

    /// Fake trainer: emit one batch point, then complete immediately.
    fn completes() -> TrainerFactory {
        Arc::new(|_params, _control, events, id| {
            tokio::spawn(async move {
                let _ = events.send((
                    id,
                    ThreadMsg::Event(TrainEvent::Batch {
                        epoch: 1,
                        batch: 1,
                        total_batches: 1,
                        cls_loss: 1.0,
                        bbox_loss: 1.0,
                        total_loss: 2.0,
                    }),
                ));
                let _ = events.send((
                    id,
                    ThreadMsg::Exited(Ok(TrainExit::Completed {
                        run_dir: PathBuf::from("runs/20260912-000000"),
                        duration_secs: 1,
                    })),
                ));
            });
        })
    }

    /// Fake trainer honoring the control channel: holds until Pause (then
    /// confirms with `TrainEvent::Paused`), exits Aborted on Abort.
    fn controllable() -> TrainerFactory {
        Arc::new(|_params, mut control, events, id| {
            tokio::spawn(async move {
                let mut paused_sent = false;
                loop {
                    let action = control.borrow().action;
                    match action {
                        TrainAction::Continue => {}
                        TrainAction::Pause => {
                            if !paused_sent {
                                paused_sent = true;
                                let _ = events.send((id, ThreadMsg::Event(TrainEvent::Paused)));
                            }
                        }
                        TrainAction::Abort => {
                            let _ = events.send((
                                id,
                                ThreadMsg::Exited(Ok(TrainExit::Aborted {
                                    run_dir: PathBuf::from("runs/20260912-000000"),
                                    duration_secs: 1,
                                })),
                            ));
                            return;
                        }
                    }
                    if control.changed().await.is_err() {
                        return;
                    }
                }
            });
        })
    }

    /// Fake trainer that fails.
    fn fails() -> TrainerFactory {
        Arc::new(|_params, _control, events, id| {
            tokio::spawn(async move {
                let _ = events.send((id, ThreadMsg::Exited(Err("boom".to_string()))));
            });
        })
    }

    async fn recv(rx: &mut broadcast::Receiver<ManagerEvent>) -> ManagerEvent {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for manager event")
            .expect("broadcast closed")
    }

    #[tokio::test]
    async fn start_rejects_second_start() {
        let handle = TrainManagerHandle::spawn_with(controllable());
        let (dir, params) = test_params();
        handle.start(params.clone(), 0.0).await.unwrap();
        let err = handle.start(params, 0.0).await.unwrap_err();
        assert!(err.contains("already active"));
        let snap = handle.snapshot(None).await.unwrap();
        assert!(matches!(
            snap.status,
            RunStatus::Active {
                phase: Phase::Running,
                ..
            }
        ));
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn start_validates_dataset() {
        let handle = TrainManagerHandle::spawn_with(completes());
        let err = handle.start(TrainParams::default(), 0.0).await.unwrap_err();
        assert!(err.contains("no dataset snapshot"));
        let (dir, mut params) = test_params();
        params.config.dataset = "missing".to_string();
        let err = handle.start(params, 0.0).await.unwrap_err();
        assert!(err.contains("not found"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn pause_then_resume() {
        let handle = TrainManagerHandle::spawn_with(controllable());
        let (dir, params) = test_params();
        let id = handle.start(params, 0.0).await.unwrap();
        let mut dump = handle.attach(id).await.unwrap();

        handle
            .command(TrainingCommand::Pause { id })
            .await
            .unwrap();
        // Instant ack, then the settled state once the thread confirms.
        assert_eq!(
            recv(&mut dump.events).await,
            ManagerEvent::PhaseChanged {
                id,
                phase: Phase::Pausing
            }
        );
        assert_eq!(
            recv(&mut dump.events).await,
            ManagerEvent::PhaseChanged {
                id,
                phase: Phase::Paused
            }
        );
        let snap = handle.snapshot(Some(id)).await.unwrap();
        assert!(matches!(
            snap.status,
            RunStatus::Active {
                phase: Phase::Paused,
                ..
            }
        ));

        handle
            .command(TrainingCommand::Resume { id })
            .await
            .unwrap();
        assert_eq!(
            recv(&mut dump.events).await,
            ManagerEvent::PhaseChanged {
                id,
                phase: Phase::Running
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn stop_ends_with_stopped_outcome() {
        let handle = TrainManagerHandle::spawn_with(controllable());
        let (dir, params) = test_params();
        let id = handle.start(params, 0.0).await.unwrap();
        let mut dump = handle.attach(id).await.unwrap();

        handle
            .command(TrainingCommand::Stop { id })
            .await
            .unwrap();
        assert_eq!(
            recv(&mut dump.events).await,
            ManagerEvent::PhaseChanged {
                id,
                phase: Phase::Stopping
            }
        );
        let outcome = match recv(&mut dump.events).await {
            ManagerEvent::Ended { id: ended_id, outcome } => {
                assert_eq!(ended_id, id);
                outcome
            }
            other => panic!("expected Ended, got {other:?}"),
        };
        assert!(matches!(outcome, Outcome::Stopped { .. }));
        let snap = handle.snapshot(Some(id)).await.unwrap();
        assert!(matches!(snap.status, RunStatus::Ended { .. }));
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn reset_wipes_run_and_allows_new_start() {
        let handle = TrainManagerHandle::spawn_with(controllable());
        let (dir, params) = test_params();
        let id = handle.start(params.clone(), 0.0).await.unwrap();
        let mut dump = handle.attach(id).await.unwrap();

        handle
            .command(TrainingCommand::Reset { id })
            .await
            .unwrap();
        assert_eq!(
            recv(&mut dump.events).await,
            ManagerEvent::Reset { id }
        );
        // The run is gone from the registry — there is no idle record to
        // snapshot or attach to.
        assert!(handle.snapshot(None).await.is_err());
        assert!(handle.snapshot(Some(id)).await.is_err());

        // New Start accepted while the old thread winds down; its late
        // Exited is dropped (its id is no longer in the registry) and must
        // not clobber the new run.
        let id2 = handle.start(params, 0.0).await.unwrap();
        assert_ne!(id, id2);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let snap = handle.snapshot(Some(id2)).await.unwrap();
        assert!(matches!(
            snap.status,
            RunStatus::Active {
                phase: Phase::Running,
                ..
            }
        ));
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn trainer_failure_ends_failed() {
        let handle = TrainManagerHandle::spawn_with(fails());
        let (dir, params) = test_params();
        let id = handle.start(params, 0.0).await.unwrap();
        // The fake fails instantly, so the `Ended` broadcast may fire before
        // any viewer can attach (attach needs the id returned by start) —
        // assert on the settled run record instead. The `Ended` broadcast
        // itself is covered by `stop_ends_with_stopped_outcome`.
        loop {
            let snap = handle.snapshot(Some(id)).await.unwrap();
            if let RunStatus::Ended {
                outcome: Outcome::Failed { error },
                ..
            } = &snap.status
            {
                assert_eq!(error, "boom");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn completed_run_replays_to_late_viewers() {
        let handle = TrainManagerHandle::spawn_with(completes());
        let (dir, params) = test_params();
        let id = handle.start(params, 0.0).await.unwrap();
        // Wait for the run to end.
        loop {
            let snap = handle.snapshot(None).await.unwrap();
            if matches!(snap.status, RunStatus::Ended { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let dump = handle.attach(id).await.unwrap();
        assert_eq!(dump.id, id);
        assert!(matches!(dump.status, RunStatus::Ended { .. }));
        assert_eq!(dump.replay.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn commands_with_unknown_id_are_rejected() {
        let handle = TrainManagerHandle::spawn_with(controllable());
        let id = Uuid::new_v4();
        assert!(handle.snapshot(None).await.is_err());
        assert!(handle.snapshot(Some(id)).await.is_err());
        assert!(handle.attach(id).await.is_err());
        assert!(handle.command(TrainingCommand::Pause { id }).await.is_err());
        assert!(handle.command(TrainingCommand::Stop { id }).await.is_err());
        assert!(
            handle
                .command(TrainingCommand::SetSpeed {
                    id,
                    batches_per_sec: 1.0,
                })
                .await
                .is_err()
        );
        // Reset with an unknown id errors too — there is no global idle run
        // to wipe.
        assert!(handle.command(TrainingCommand::Reset { id }).await.is_err());
        // Start/Attach are not runtime commands.
        let err = handle
            .command(TrainingCommand::Attach { id })
            .await
            .unwrap_err();
        assert!(err.contains("not a runtime command"));
    }

    #[tokio::test]
    async fn start_rejects_tiny_snapshot() {
        let handle = TrainManagerHandle::spawn_with(completes());
        let (dir, params) = test_params();
        let manifest = serde_json::json!({
            "name": "test",
            "created_at": "2026-01-01T00:00:00Z",
            "entries": [{"image_id": uuid::Uuid::new_v4(), "labels": []}],
        });
        std::fs::write(dir.join("datasets/test.json"), manifest.to_string()).unwrap();
        let err = handle.start(params, 0.0).await.unwrap_err();
        assert!(err.contains("need at least 2"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn replay_caps_batch_events_but_keeps_epochs() {
        let batches_per_epoch = MAX_REPLAY_BATCH_EVENTS / 2 + 100;
        let handle = TrainManagerHandle::spawn_with(Arc::new(
            move |_params, _control, events, id| {
                tokio::spawn(async move {
                    for epoch in 1..=2usize {
                        for batch in 1..=batches_per_epoch {
                            let _ = events.send((
                                id,
                                ThreadMsg::Event(TrainEvent::Batch {
                                    epoch,
                                    batch,
                                    total_batches: 1,
                                    cls_loss: 1.0,
                                    bbox_loss: 1.0,
                                    total_loss: 2.0,
                                }),
                            ));
                        }
                        let _ = events.send((
                            id,
                            ThreadMsg::Event(TrainEvent::EpochEnd {
                                epoch,
                                total_epochs: 2,
                                train_cls: 1.0,
                                train_bbox: 1.0,
                                valid_cls: 1.0,
                                valid_bbox: 1.0,
                            }),
                        ));
                    }
                    let _ = events.send((
                        id,
                        ThreadMsg::Exited(Ok(TrainExit::Completed {
                            run_dir: PathBuf::from("runs/20260912-000000"),
                            duration_secs: 1,
                        })),
                    ));
                });
            },
        ));
        let (dir, params) = test_params();
        let id = handle.start(params, 0.0).await.unwrap();
        loop {
            let snap = handle.snapshot(None).await.unwrap();
            if matches!(snap.status, RunStatus::Ended { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let dump = handle.attach(id).await.unwrap();
        let batches = dump
            .replay
            .iter()
            .filter(|e| matches!(e, TrainEvent::Batch { .. }))
            .count();
        let epochs = dump
            .replay
            .iter()
            .filter(|e| matches!(e, TrainEvent::EpochEnd { .. }))
            .count();
        assert_eq!(batches, MAX_REPLAY_BATCH_EVENTS);
        assert_eq!(epochs, 2);
        // Oldest batch points were dropped first.
        match dump.replay.first() {
            Some(TrainEvent::Batch { epoch, batch, .. }) => {
                assert_eq!((*epoch, *batch), (1, 201));
            }
            other => panic!("expected oldest surviving Batch, got {other:?}"),
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
