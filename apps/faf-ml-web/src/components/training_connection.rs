//! WebSocket connection to the training server (`/ws/training`), modeled on
//! fafcn-web's `websocket_service.rs`.

use std::{cell::RefCell, rc::Rc};

use faf_ml_core::{TrainingCommand, TrainingConfig, TrainingEvent};
use uuid::Uuid;
use wasm_bindgen::prelude::*;
use web_sys::{CloseEvent, Event, MessageEvent, WebSocket};

/// Run id carried by a run-scoped `TrainingEvent` (`None` for `Error`).
fn event_run_id(event: &TrainingEvent) -> Option<Uuid> {
    match event {
        TrainingEvent::Started { id }
        | TrainingEvent::Metrics { id, .. }
        | TrainingEvent::Status { id, .. }
        | TrainingEvent::Cleared { id }
        | TrainingEvent::Finished { id } => Some(*id),
        TrainingEvent::Error { .. } => None,
    }
}

/// Connection handle to the training server.
#[derive(Clone)]
pub struct TrainingConnection {
    ws: WebSocket,
    /// Server-assigned run id (set by `TrainingEvent::Started`, or known up
    /// front when attaching). Scopes every command sent on this socket.
    run_id: Rc<RefCell<Option<Uuid>>>,
}

impl TrainingConnection {
    /// `on_message` is called for every run-scoped `TrainingEvent`
    /// (`Metrics`/`Status`/`Cleared`). `on_status` is called when the
    /// connection opens/closes/errors or the run finishes. Callbacks are
    /// intentionally `.forget()`ed — one connection per page, replaced when a
    /// new run starts (same trade-off as fafcn's `SimConnection`).
    ///
    /// Open a WebSocket and START a new training run; the run id is stored
    /// once the server replies `TrainingEvent::Started`.
    pub fn open(
        config: TrainingConfig,
        speed: f64,
        on_message: impl FnMut(TrainingEvent) + 'static,
        on_status: impl FnMut(String) + 'static,
    ) -> Result<Self, JsValue> {
        let start_text =
            serde_json::to_string(&TrainingCommand::Start { config, speed }).unwrap_or_default();
        Self::connect(start_text, None, on_message, on_status)
    }

    /// Open a WebSocket and ATTACH to run `id` (replay + live stream; starts
    /// nothing). Used when the page loads mid-run; the id comes from
    /// `GET /api/training/status`.
    pub fn open_attach(
        id: Uuid,
        on_message: impl FnMut(TrainingEvent) + 'static,
        on_status: impl FnMut(String) + 'static,
    ) -> Result<Self, JsValue> {
        Self::connect(
            serde_json::to_string(&TrainingCommand::Attach { id }).unwrap_or_default(),
            Some(id),
            on_message,
            on_status,
        )
    }

    fn connect(
        start_text: String,
        known_run_id: Option<Uuid>,
        on_message: impl FnMut(TrainingEvent) + 'static,
        on_status: impl FnMut(String) + 'static,
    ) -> Result<Self, JsValue> {
        let url = crate::net::ws_url("/ws/training");
        let ws = WebSocket::new(&url)?;

        // Wrap callbacks so multiple closures can share them.
        let on_message = Rc::new(RefCell::new(on_message));
        let on_status = Rc::new(RefCell::new(on_status));
        let run_id = Rc::new(RefCell::new(known_run_id));

        let status = on_status.clone();
        let onopen = Closure::wrap(Box::new(move |e: Event| {
            if let Some(socket) = e.target().and_then(|t| t.dyn_into::<WebSocket>().ok()) {
                let _ = socket.send_with_str(&start_text);
            }
            (status.borrow_mut())("connected".to_string());
        }) as Box<dyn FnMut(_)>);
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();

        let status = on_status.clone();
        let onerror = Closure::wrap(Box::new(move |_: Event| {
            (status.borrow_mut())("error".to_string());
        }) as Box<dyn FnMut(_)>);
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();

        let status = on_status.clone();
        let onclose = Closure::wrap(Box::new(move |_: CloseEvent| {
            (status.borrow_mut())("finished".to_string());
        }) as Box<dyn FnMut(_)>);
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();

        let message = on_message.clone();
        let status = on_status.clone();
        let run = run_id.clone();
        let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Some(text) = e.data().as_string() {
                match serde_json::from_str::<TrainingEvent>(&text) {
                    Ok(TrainingEvent::Started { id }) => {
                        *run.borrow_mut() = Some(id);
                    }
                    Ok(TrainingEvent::Error { message }) => {
                        (status.borrow_mut())(format!("error: {message}"))
                    }
                    Ok(event) => {
                        // Defensive id filter (the server filters too): drop
                        // events for any run but this connection's.
                        let matches_run = event_run_id(&event).is_some_and(|id| {
                            run.borrow().is_some_and(|run_id| run_id == id)
                        });
                        if matches_run {
                            match event {
                                TrainingEvent::Finished { .. } => {
                                    (status.borrow_mut())("finished".to_string())
                                }
                                msg => (message.borrow_mut())(msg),
                            }
                        }
                    }
                    Err(err) => (status.borrow_mut())(format!("parse error: {err}")),
                }
            }
        }) as Box<dyn FnMut(_)>);
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();

        Ok(Self { ws, run_id })
    }

    /// Send a run-scoped command; `build` gets the stored run id baked in.
    /// No-op until the server assigns the id (`TrainingEvent::Started`).
    pub fn send_command(&self, build: impl FnOnce(Uuid) -> TrainingCommand) {
        if let Some(id) = *self.run_id.borrow() {
            let _ = self
                .ws
                .send_with_str(&serde_json::to_string(&build(id)).unwrap_or_default());
        }
    }
}
