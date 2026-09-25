//! Generic AntV G6 graph viewer component.
//!
//! This crate renders a [`GraphData`] payload through the `window.fafG6` JS
//! bridge. The host application is responsible for loading AntV G6 and the
//! bridge script (`faf_g6_bridge.js`) before this component is mounted.
//!
//! The component accepts generic node/edge payloads, so callers can attach any
//! app-specific data (e.g. unit summaries) to each node and look it up from the
//! returned node id in the click handler.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use dioxus::prelude::*;
use js_sys::Reflect;
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{console, CustomEvent, Event};

/// Generic node descriptor for G6 rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNodeData<N = ()> {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<i32>,
    /// Optional image URL rendered inside the node (e.g. a unit portrait).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Visually highlight this node (e.g. the focused unit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<N>,
}

/// Generic edge descriptor for G6 rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEdgeData<E = ()> {
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub dashed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<E>,
}

/// Generic graph payload for G6 rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphData<N = (), E = ()> {
    pub nodes: Vec<GraphNodeData<N>>,
    pub edges: Vec<GraphEdgeData<E>>,
}

impl<N, E> GraphData<N, E> {
    /// Build graph data from a [`petgraph::DiGraph`].
    ///
    /// `node_fn` and `edge_fn` convert the caller's node/edge weights into the
    /// G6 wire format. The returned edge descriptors have their `source` and
    /// `target` fields populated from the petgraph topology. The mapped payloads
    /// `MN` and `ME` may differ from the original weight types.
    pub fn from_petgraph<FN, FE, MN, ME>(
        graph: &DiGraph<N, E>,
        mut node_fn: FN,
        mut edge_fn: FE,
    ) -> GraphData<MN, ME>
    where
        FN: FnMut(&N) -> GraphNodeData<MN>,
        FE: FnMut(&E) -> GraphEdgeData<ME>,
    {
        let index_to_id: HashMap<petgraph::graph::NodeIndex, String> = graph
            .node_indices()
            .map(|idx| (idx, node_fn(&graph[idx]).id.clone()))
            .collect();

        let nodes = graph.node_weights().map(node_fn).collect();
        let edges = graph
            .edge_references()
            .map(|e| {
                let mut edge = edge_fn(e.weight());
                edge.source = index_to_id[&e.source()].clone();
                edge.target = index_to_id[&e.target()].clone();
                edge
            })
            .collect();

        GraphData { nodes, edges }
    }

    /// Look up a node by its `id`.
    pub fn node_by_id(&self, id: &str) -> Option<&GraphNodeData<N>> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

type ClickListener = Rc<RefCell<Option<Closure<dyn FnMut(Event)>>>>;

/// Render an interactive graph with AntV G6.
///
/// `id` is the DOM container id. `on_node_click` receives the clicked node's
/// `id`; the caller can use [`GraphData::node_by_id`] to resolve app-specific
/// payloads.
#[component]
pub fn GraphView<N, E>(
    id: String,
    data: GraphData<N, E>,
    on_node_click: EventHandler<String>,
) -> Element
where
    N: Serialize + Clone + PartialEq + 'static,
    E: Serialize + Clone + PartialEq + 'static,
{
    let listener: ClickListener = use_hook(|| Rc::new(RefCell::new(None)));
    let listener_for_effect = listener.clone();
    let listener_for_drop = listener.clone();
    let effect_id = id.clone();

    // Track `data` as a reactive dependency so the graph re-initializes
    // whenever the caller passes new data (e.g. switching faction tabs).
    use_effect(use_reactive!(|data| {
        // Attach the custom-event listener only once.
        if listener_for_effect.borrow().is_none() {
            let on_click = on_node_click;
            let closure = Closure::wrap(Box::new(move |event: Event| {
                if let Some(custom) = event.dyn_ref::<CustomEvent>() {
                    if let Some(detail) = custom.detail().as_string() {
                        on_click.call(detail);
                    }
                }
            }) as Box<dyn FnMut(_)>);

            if let Some(document) = web_sys::window().and_then(|w| w.document()) {
                let _ = document.add_event_listener_with_callback(
                    "faf:g6-node-click",
                    closure.as_ref().unchecked_ref(),
                );
            }
            *listener_for_effect.borrow_mut() = Some(closure);
        }

        // (Re-)initialize the graph whenever this effect runs.
        if let Ok(json) = serde_json::to_string(&data) {
            console::log_1(&"[GraphView] initializing G6".into());
            if let Err(err) = init_g6(&effect_id, &json) {
                console::error_1(&format!("[GraphView] init failed: {err:?}").into());
            }
        }
    }));

    use_drop(move || {
        if let Some(closure) = listener_for_drop.borrow_mut().take() {
            if let Some(document) = web_sys::window().and_then(|w| w.document()) {
                let _ = document.remove_event_listener_with_callback(
                    "faf:g6-node-click",
                    closure.as_ref().unchecked_ref(),
                );
            }
        }
        destroy_g6();
    });

    rsx! {
        div { id: "{id}", class: "w-full h-full" }
    }
}

fn init_g6(container_id: &str, json: &str) -> Result<(), ()> {
    let window = web_sys::window().ok_or(())?;
    let faf_g6 = Reflect::get(&window, &"fafG6".into()).map_err(|_| ())?;
    let init = Reflect::get(&faf_g6, &"init".into()).map_err(|_| ())?;
    let init_fn = init.dyn_into::<js_sys::Function>().map_err(|_| ())?;
    init_fn
        .call2(&faf_g6, &container_id.into(), &json.into())
        .map_err(|_| ())?;
    Ok(())
}

fn destroy_g6() {
    if let Some(window) = web_sys::window() {
        if let Ok(faf_g6) = Reflect::get(&window, &"fafG6".into()) {
            if let Ok(destroy) = Reflect::get(&faf_g6, &"destroy".into()) {
                if let Ok(destroy_fn) = destroy.dyn_into::<js_sys::Function>() {
                    let _ = destroy_fn.call0(&faf_g6);
                }
            }
        }
    }
}
