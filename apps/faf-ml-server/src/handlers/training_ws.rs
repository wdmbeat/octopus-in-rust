//! Training routes: `GET /ws/training` (start/attach + live stream) and
//! `GET /api/training/status` (the run registry as JSON).
//!
//! This handler is a pure protocol adapter: the training manager actor
//! (`faf-ml-model::manager`, held in `AppState::training`) owns the runs; the
//! handler maps wire types ↔ manager types, replays buffered metrics,
//! forwards live broadcast events, and relays commands. Runs are addressed
//! by server-assigned ids: a socket either starts a run (`Started{id}` ack)
//! or attaches to one, and then only receives events for that run. A client
//! disconnect ends only the forwarding — training always continues
//! server-side.

use axum::{
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
    Json,
};
use faf_ml_core::{
    TrainingCommand, TrainingEvent, TrainingMetricsPoint, TrainingRunStatus, TrainingStatus,
};
use faf_ml_model::{
    manager::{ManagerEvent, Outcome, Phase, RunStatus},
    train::{TrainEvent, TrainParams},
};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    state::AppState,
};

/// The seperation of faf-ml-model (training engine) vs faf-ml-core (protocol shared by server, mcp client and web ui)
/// Manager phase → wire status for an active run.
fn wire_phase(phase: Phase) -> TrainingStatus {
    match phase {
        Phase::Running => TrainingStatus::Running,
        Phase::Pausing => TrainingStatus::Pausing,
        Phase::Paused => TrainingStatus::Paused,
        Phase::Stopping => TrainingStatus::Stopping,
    }
}

/// The seperation of faf-ml-model (training engine) vs faf-ml-core (protocol shared by server, mcp client and web ui)
/// Terminal outcome → wire status.
fn wire_outcome(outcome: Outcome) -> TrainingStatus {
    match outcome {
        Outcome::Completed {
            run_dir,
            duration_secs,
        } => TrainingStatus::Done {
            run_dir: run_dir.display().to_string(),
            duration_secs,
        },
        Outcome::Stopped {
            run_dir,
            duration_secs,
        } => TrainingStatus::Stopped {
            run_dir: run_dir.display().to_string(),
            duration_secs,
        },
        Outcome::Failed { error } => TrainingStatus::Failed { error },
    }
}

/// Manager run status → wire status.
fn wire_status(status: &RunStatus) -> TrainingStatus {
    match status {
        RunStatus::Active { phase, .. } => wire_phase(*phase),
        RunStatus::Ended { outcome, .. } => wire_outcome(outcome.clone()),
    }
}

/// One `TrainEvent` → one chart point (`seq` is assigned here, per attached
/// viewer, continuing after the replay). `Paused` carries no metrics.
fn metrics_point(seq: u64, event: &TrainEvent) -> Option<TrainingMetricsPoint> {
    match event {
        TrainEvent::Batch {
            epoch,
            batch,
            total_batches,
            cls_loss,
            bbox_loss,
            total_loss,
        } => Some(TrainingMetricsPoint {
            seq,
            epoch: *epoch,
            batch: *batch,
            total_batches: *total_batches,
            train_loss: *total_loss as f64,
            cls_loss: *cls_loss as f64,
            bbox_loss: *bbox_loss as f64,
            valid_loss: None,
            map: None,
        }),
        TrainEvent::EpochEnd {
            epoch,
            train_cls,
            train_bbox,
            valid_cls,
            valid_bbox,
            ..
        } => Some(TrainingMetricsPoint {
            seq,
            epoch: *epoch,
            batch: 0, // epoch-eval point (progress line shows latest batch point anyway)
            total_batches: 0,
            train_loss: (train_cls + train_bbox) as f64,
            cls_loss: *train_cls as f64,
            bbox_loss: *train_bbox as f64,
            valid_loss: Some((valid_cls + valid_bbox) as f64),
            map: None,
        }),
        TrainEvent::Paused => None,
    }
}

/// `GET /api/training/status` — the active or most recent run (404 when the
/// registry is empty).
/// TODO: change snapshot to use Uuid instead of Option
pub async fn get_training_status(State(state): State<AppState>) -> Result<Json<TrainingRunStatus>> {
    let snapshot = state
        .training
        .snapshot(None)
        .await
        .map_err(|_| Error::NotFound)?;
    let (params, started_at) = match &snapshot.status {
        RunStatus::Active {
            config, started_at, ..
        }
        | RunStatus::Ended {
            config, started_at, ..
        } => (config.clone(), *started_at),
    };
    let points = snapshot.replay.len();
    let latest = snapshot
        .replay
        .last()
        .and_then(|event| metrics_point(points as u64, event));
    Ok(Json(TrainingRunStatus {
        id: snapshot.id,
        config: params.config.clone(),
        started_at,
        status: wire_status(&snapshot.status),
        points,
        latest,
    }))
}

/// Upgrade an HTTP connection to a WebSocket and view/start a training run.
pub async fn training_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: axum::extract::ws::WebSocket, state: AppState) {
    use axum::extract::ws::Message;

    // First frame decides: start a new run or attach to an existing one.
    let run_id: Uuid = loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<TrainingCommand>(&text) {
                    Ok(TrainingCommand::Start { config, speed }) => {
                        // The server injects the store paths around the
                        // client-supplied config.
                        let params = TrainParams {
                            config,
                            data: (*state.data_dir).clone(),
                            out: state.data_dir.join("runs"),
                        };
                        match state.training.start(params, speed).await {
                            Ok(id) => {
                                if send_json(&mut socket, &TrainingEvent::Started { id })
                                    .await
                                    .is_err()
                                {
                                    // TODO: Here the client never received training id, so it's effectively leaked because client will never use the id to attach again.
                                    return;
                                }

                                break id;
                            }
                            Err(e) => {
                                let _ =
                                    send_json(&mut socket, &TrainingEvent::Error { message: e })
                                        .await;
                                return;
                            }
                        }
                    }
                    Ok(TrainingCommand::Attach { id }) => break id,
                    Ok(_) => {
                        let _ = send_json(
                            &mut socket,
                            &TrainingEvent::Error {
                                message: "expected Start or Attach before commands".to_string(),
                            },
                        )
                        .await;
                    }
                    Err(e) => {
                        let _ = send_json(
                            &mut socket,
                            &TrainingEvent::Error {
                                message: format!("invalid message: {e}"),
                            },
                        )
                        .await;
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => return,
            _ => continue,
        }
    };

    // Replay + live subscription, taken atomically by the manager.
    let dump = match state.training.attach(run_id).await {
        Ok(dump) => dump,
        Err(e) => {
            let _ = send_json(&mut socket, &TrainingEvent::Error { message: e }).await;
            return;
        }
    };
    let status = wire_status(&dump.status);
    let mut seq = 0u64;
    for event in &dump.replay {
        seq += 1;
        if let Some(point) = metrics_point(seq, event) {
            if send_json(&mut socket, &TrainingEvent::Metrics { id: run_id, point })
                .await
                .is_err()
            {
                return;
            }
        }
    }
    let terminal = matches!(
        &status,
        TrainingStatus::Done { .. }
            | TrainingStatus::Stopped { .. }
            | TrainingStatus::Failed { .. }
    );
    if send_json(&mut socket, &TrainingEvent::Status { id: run_id, status })
        .await
        .is_err()
    {
        return;
    }
    if terminal {
        // Attaching to an ended run replays it, then closes like a live end.
        let _ = send_json(&mut socket, &TrainingEvent::Finished { id: run_id }).await;
        return;
    }

    // Forward live events + relay commands until the client leaves or the
    // run reaches a terminal status.
    let mut events = dump.events;
    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        // One broadcast channel serves all runs — this socket
                        // only forwards its own run's events.
                        let msg = match event {
                            ManagerEvent::Train { id, event } if id == run_id => {
                                seq += 1;
                                metrics_point(seq, &event)
                                    .map(|point| TrainingEvent::Metrics { id, point })
                            }
                            ManagerEvent::PhaseChanged { id, phase } if id == run_id => {
                                Some(TrainingEvent::Status {
                                    id,
                                    status: wire_phase(phase),
                                })
                            }
                            ManagerEvent::Ended { id, outcome } if id == run_id => {
                                Some(TrainingEvent::Status {
                                    id,
                                    status: wire_outcome(outcome),
                                })
                            }
                            ManagerEvent::Reset { id } if id == run_id => {
                                Some(TrainingEvent::Cleared { id })
                            }
                            _ => None,
                        };
                        let Some(msg) = msg else { continue };
                        let terminal = matches!(
                            &msg,
                            TrainingEvent::Status {
                                status: TrainingStatus::Done { .. }
                                    | TrainingStatus::Stopped { .. }
                                    | TrainingStatus::Failed { .. },
                                ..
                            }
                        );
                        if send_json(&mut socket, &msg).await.is_err() {
                            return;
                        }
                        if terminal {
                            let _ =
                                send_json(&mut socket, &TrainingEvent::Finished { id: run_id })
                                    .await;
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<TrainingCommand>(&text) {
                            Ok(TrainingCommand::Start { .. }) => {
                                let _ = send_json(
                                    &mut socket,
                                    &TrainingEvent::Error {
                                        message: "already started".to_string(),
                                    },
                                )
                                .await;
                            }
                            Ok(TrainingCommand::Attach { .. }) => {
                                let _ = send_json(
                                    &mut socket,
                                    &TrainingEvent::Error {
                                        message: "already attached".to_string(),
                                    },
                                )
                                .await;
                            }
                            Ok(cmd) => {
                                // The run id comes from the message, not the
                                // socket — forward unchanged.
                                if let Err(e) = state.training.command(cmd).await {
                                    let _ = send_json(
                                        &mut socket,
                                        &TrainingEvent::Error { message: e },
                                    )
                                    .await;
                                }
                            }
                            Err(e) => {
                                let _ = send_json(
                                    &mut socket,
                                    &TrainingEvent::Error {
                                        message: format!("invalid message: {e}"),
                                    },
                                )
                                .await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return,
                    _ => {}
                }
            }
        }
    }
}

async fn send_json(
    socket: &mut axum::extract::ws::WebSocket,
    message: &TrainingEvent,
) -> Result<()> {
    let text = serde_json::to_string(message).unwrap_or_default();
    socket
        .send(axum::extract::ws::Message::Text(text.into()))
        .await
        .map_err(|e| Error::Internal(e.to_string()))
}
