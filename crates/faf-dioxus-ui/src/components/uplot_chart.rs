//! High-performance time-series chart using uPlot.
//!
//! This component renders a tab-switchable line chart backed by the uPlot
//! JavaScript library. It is designed for real-time streaming data: new points
//! are passed to uPlot via `setData`, which handles incremental rendering
//! internally.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dioxus::prelude::*;
use js_sys::{Array, Function, Object, Reflect};
pub use plotters::prelude::RGBColor;
use wasm_bindgen::prelude::*;
use web_sys::{window, ResizeObserver, ResizeObserverEntry};

static CHART_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Owned uPlot instance plus the closures/observers that must stay alive with it.
struct ChartHandle {
    index: usize,
    chart: JsValue,
    // Kept alive so the uPlot hooks/closures remain valid.
    #[allow(dead_code)]
    hooks: Vec<Closure<dyn FnMut(JsValue)>>,
    observer: Option<ResizeObserver>,
    _observer_closure: Option<ObserverClosure>,
}

type ObserverClosure = Closure<dyn FnMut(Vec<ResizeObserverEntry>)>;

/// Tooltip content signal: `(time_label, vec![(series_label, value_label)])`.
type TooltipSignal = Signal<Option<(String, Vec<(String, String)>)>>;

/// Signals used to drive the hover tooltip: its content and pixel position.
#[derive(Clone, Copy)]
struct TooltipSignals {
    content: TooltipSignal,
    pos: Signal<(f64, f64)>,
}

/// Wrapper around a function pointer so it can be used as a Dioxus prop
/// without triggering function-pointer equality warnings.
pub struct ChartMetric<T>(fn(&T) -> f64);

impl<T> ChartMetric<T> {
    pub fn new(extractor: fn(&T) -> f64) -> Self {
        Self(extractor)
    }

    pub(crate) fn extract(&self, value: &T) -> f64 {
        (self.0)(value)
    }
}

impl<T> Clone for ChartMetric<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ChartMetric<T> {}

impl<T> PartialEq for ChartMetric<T> {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Configuration for one data series inside a chart tab.
#[derive(Clone, PartialEq)]
pub struct ChartSeries<T> {
    /// Label shown in the tooltip and legend.
    pub label: String,
    /// Color used for the line.
    pub color: RGBColor,
    /// Extracts the y value for a data point.
    pub y_extractor: ChartMetric<T>,
    /// Optional dash pattern for the line, e.g. `vec![4.0, 4.0]` for a dashed line.
    pub dash: Option<Vec<f64>>,
    /// Connect the line across `null` (gap) points — needed for sparse
    /// series (e.g. epoch-end eval metrics), whose isolated points would
    /// otherwise render as nothing.
    pub span_gaps: bool,
}

impl<T> ChartSeries<T> {
    pub fn new(label: impl Into<String>, color: RGBColor, y_extractor: ChartMetric<T>) -> Self {
        Self {
            label: label.into(),
            color,
            y_extractor,
            dash: None,
            span_gaps: false,
        }
    }

    pub fn with_dash(mut self, dash: impl Into<Vec<f64>>) -> Self {
        self.dash = Some(dash.into());
        self
    }

    pub fn with_span_gaps(mut self) -> Self {
        self.span_gaps = true;
        self
    }
}

/// Configuration for one tab/metric in the chart panel.
#[derive(Clone, PartialEq)]
pub struct ChartTab<T> {
    /// Label shown on the tab.
    pub label: String,
    /// Series plotted together on this tab.
    pub series: Vec<ChartSeries<T>>,
}

/// High-performance time-series chart backed by uPlot.
///
/// The component renders a single canvas. Tabs switch which metric is
/// displayed. Data updates are forwarded to uPlot through `setData`, so the
/// library can render new points efficiently without a full redraw from Rust.
#[component]
pub fn UplotChart<T: Clone + PartialEq + 'static>(
    data: Signal<Vec<T>>,
    x_extractor: ChartMetric<T>,
    tabs: Vec<ChartTab<T>>,
    #[props(default)] sidebar: Option<Element>,
) -> Element {
    let mut selected_index = use_signal(|| 0usize);
    let tabs_for_visibility = tabs.clone();
    let visibility = use_signal(move || {
        tabs_for_visibility
            .iter()
            .map(|tab| vec![true; tab.series.len()])
            .collect::<Vec<Vec<bool>>>()
    });
    let chart_id = use_hook(|| {
        let id = CHART_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("uplot-chart-{id}")
    });
    // Stores the active tab index, the live uPlot instance, and any closures
    // installed as hooks so they stay alive as long as the chart.
    let chart_state: Rc<RefCell<Option<ChartHandle>>> = use_hook(|| Rc::new(RefCell::new(None)));
    let chart_state_for_effect = chart_state.clone();
    let chart_state_for_cleanup = chart_state.clone();

    // Tooltip content and pixel position within the chart.
    // The tuple is (time_label, vec![(series_label, value_label)]).
    let tooltip: TooltipSignal = use_signal(|| None);
    let tooltip_pos = use_signal(|| (0.0_f64, 0.0_f64));

    use_drop(move || {
        if let Some(handle) = chart_state_for_cleanup.borrow_mut().take() {
            if let Some(observer) = handle.observer {
                observer.disconnect();
            }
            let _ = destroy_chart(&handle.chart);
        }
    });

    let tabs_for_effect = tabs.clone();
    let chart_id_for_effect = chart_id.clone();
    let visibility_for_effect = visibility;
    use_effect(move || {
        let points = data.read();
        let tab_index = *selected_index.read();
        let Some(tab) = tabs_for_effect.get(tab_index) else {
            return;
        };

        let mut state = chart_state_for_effect.borrow_mut();
        let should_create = match state.as_ref() {
            Some(handle) => handle.index != tab_index,
            None => true,
        };

        if should_create {
            if let Some(old_handle) = state.take() {
                if let Some(observer) = old_handle.observer {
                    observer.disconnect();
                }
                let _ = destroy_chart(&old_handle.chart);
            }
            if let Some(handle) = create_chart(
                &chart_id_for_effect,
                data,
                x_extractor,
                &tab.series,
                TooltipSignals {
                    content: tooltip,
                    pos: tooltip_pos,
                },
                visibility_for_effect,
                tab_index,
            ) {
                *state = Some(ChartHandle {
                    index: tab_index,
                    ..handle
                });
            }
            drop(state);
            // Apply the stored visibility state to the newly created chart.
            if let Some(handle) = chart_state_for_effect.borrow().as_ref() {
                if let Some(tab_vis) = visibility_for_effect.read().get(tab_index) {
                    for (i, show) in tab_vis.iter().enumerate() {
                        let _ = set_series_visibility(&handle.chart, i + 1, *show);
                    }
                }
            }
        } else if let Some(handle) = state.as_ref() {
            let _ = update_chart(&handle.chart, &points, x_extractor, &tab.series);
        }
    });

    if tabs.is_empty() {
        return rsx! {
            div { class: "flex-1 flex items-center justify-center",
                p { class: "text-sm text-neutral-500", "No chart tabs configured." }
            }
        };
    }

    let active_tab_index = *selected_index.read();
    let active_tab = tabs
        .get(active_tab_index)
        .cloned()
        .unwrap_or_else(|| tabs[0].clone());
    let chart_state_for_legend = chart_state.clone();
    let series_visibility: Vec<bool> = active_tab
        .series
        .iter()
        .enumerate()
        .map(|(i, _)| {
            visibility
                .read()
                .get(active_tab_index)
                .and_then(|v| v.get(i).copied())
                .unwrap_or(true)
        })
        .collect();

    rsx! {
        document::Style {
            r#"
            .uplot-chart-container .uplot {{
                width: 100%;
                height: 100%;
            }}
            .uplot-chart-container .uplot .title {{
                color: #ffffff;
            }}

            "#
        }
        div { class: "flex-1 flex flex-col min-h-0",
            div { class: "flex gap-1 mb-2 shrink-0 flex-wrap justify-center",
                for (index , tab) in tabs.iter().enumerate() {
                    TabButton {
                        label: tab.label.clone(),
                        is_active: *selected_index.read() == index,
                        onclick: move |_| selected_index.set(index),
                    }
                }
            }
            div { class: "flex-1 rounded-lg border border-neutral-800 bg-[#171717] p-2 min-h-0 overflow-hidden flex flex-col",
                div { class: "flex-1 min-h-0 flex flex-row gap-3",
                    if let Some(sidebar) = sidebar.as_ref() {
                        div { class: "shrink-0 self-start overflow-y-auto w-56",
                            { sidebar.clone() }
                        }
                    }
                    div { class: "flex-1 min-h-0 flex flex-col",
                        div { class: "flex items-center justify-center gap-4 mb-1 shrink-0 flex-wrap",
                            for (series_index , s) in active_tab.series.iter().enumerate() {
                                LegendItem {
                                    key: "{series_index}",
                                    color: rgb_to_hex(s.color),
                                    label: s.label.clone(),
                                    visible: series_visibility[series_index],
                                    onclick: {
                                        let chart_state = chart_state_for_legend.clone();
                                        let mut visibility = visibility;
                                        move |_| {
                                            let new_show = visibility.with_mut(|v| {
                                                v.get_mut(active_tab_index)
                                                    .and_then(|tab_vis| tab_vis.get_mut(series_index))
                                                    .map(|show| {
                                                        *show = !*show;
                                                        *show
                                                    })
                                            });
                                            if let Some(show) = new_show {
                                                if let Some(handle) = chart_state.borrow().as_ref() {
                                                    if handle.index == active_tab_index {
                                                        let _ = set_series_visibility(
                                                            &handle.chart,
                                                            series_index + 1,
                                                            show,
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    },
                                }
                            }
                        }
                        div { class: "flex-1 min-h-0 relative",
                            div {
                                id: "{chart_id}",
                                class: "absolute inset-0 uplot-chart-container",
                            }
                            if let Some((time, values)) = tooltip.read().as_ref() {
                                div {
                                    class: "absolute z-10 px-2 py-1 rounded bg-neutral-900 border border-neutral-700 text-xs text-white shadow pointer-events-none",
                                    style: "left: {tooltip_pos.read().0 + 12.0}px; top: {tooltip_pos.read().1 - 64.0}px;",
                                    div { "{time}" }
                                    for (label , value) in values.iter() {
                                        div { "{label}: {value}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TabButton(label: String, is_active: bool, onclick: EventHandler<()>) -> Element {
    let base = "px-3 py-1.5 text-sm rounded transition-colors";
    let active_class = if is_active {
        "bg-blue-700 text-white"
    } else {
        "bg-neutral-800 text-neutral-300 hover:bg-neutral-700 border border-neutral-700"
    };
    rsx! {
        button {
            class: "{base} {active_class}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
fn LegendItem(
    color: String,
    label: String,
    #[props(default = true)] visible: bool,
    #[props(default)] onclick: EventHandler<()>,
) -> Element {
    let state_class = if visible { "opacity-100" } else { "opacity-50" };
    let check_class = if visible {
        "bg-blue-600 border-blue-600 text-white"
    } else {
        "bg-transparent border-neutral-500 text-transparent"
    };
    let label_class = if visible { "" } else { "line-through" };
    rsx! {
        button {
            class: "flex items-center gap-1.5 {state_class} hover:opacity-80 transition-opacity",
            onclick: move |_| onclick.call(()),
            div { class: "w-3.5 h-3.5 rounded border flex items-center justify-center text-[10px] {check_class}",
                if visible { "✓" }
            }
            span {
                class: "text-lg leading-none select-none",
                style: "color: {color}",
                "▬"
            }
            span { class: "text-xs text-neutral-300 {label_class}", "{label}" }
        }
    }
}

fn create_chart<T: Clone + 'static>(
    element_id: &str,
    data: Signal<Vec<T>>,
    x_extractor: ChartMetric<T>,
    series: &[ChartSeries<T>],
    tooltip: TooltipSignals,
    visibility: Signal<Vec<Vec<bool>>>,
    tab_index: usize,
) -> Option<ChartHandle> {
    let window = window()?;
    let document = window.document()?;
    let container = document.get_element_by_id(element_id)?;

    let points = data.read();
    let series_data = build_series_data(&points, x_extractor, series);
    let width = container.client_width().max(1) as f64;
    let height = container.client_height().max(1) as f64;

    let set_cursor =
        build_set_cursor_closure(data, x_extractor, series, tooltip, visibility, tab_index);
    let opts = build_opts(series, width, height, &set_cursor);

    let uplot = Reflect::get(&window, &"uPlot".into()).ok()?;
    let uplot_fn = Function::from(uplot);
    let args = Array::new();
    args.push(&opts);
    args.push(&series_data);
    args.push(&container);
    let chart = Reflect::construct(&uplot_fn, &args).ok()?;

    let (observer, _observer_closure) = create_resize_observer(&chart, &container);

    let hooks = vec![set_cursor];
    Some(ChartHandle {
        index: 0,
        chart,
        hooks,
        observer,
        _observer_closure,
    })
}

fn update_chart<T: Clone>(
    chart: &JsValue,
    data: &[T],
    x_extractor: ChartMetric<T>,
    series: &[ChartSeries<T>],
) -> Result<(), JsValue> {
    let series_data = build_series_data(data, x_extractor, series);
    let set_data = Reflect::get(chart, &"setData".into())?;
    let set_data_fn = Function::from(set_data);
    Reflect::apply(&set_data_fn, chart, &Array::of1(&series_data))?;
    Ok(())
}

fn destroy_chart(chart: &JsValue) -> Result<(), JsValue> {
    let destroy = Reflect::get(chart, &"destroy".into())?;
    let destroy_fn = Function::from(destroy);
    Reflect::apply(&destroy_fn, chart, &Array::new())?;
    Ok(())
}

fn resize_chart(chart: &JsValue, width: f64, height: f64) -> Result<(), JsValue> {
    let set_size = Reflect::get(chart, &"setSize".into())?;
    let set_size_fn = Function::from(set_size);
    let size = Object::new();
    Reflect::set(&size, &"width".into(), &JsValue::from_f64(width))?;
    Reflect::set(&size, &"height".into(), &JsValue::from_f64(height))?;
    Reflect::apply(&set_size_fn, chart, &Array::of1(&size))?;
    Ok(())
}

fn set_series_visibility(chart: &JsValue, series_idx: usize, show: bool) -> Result<(), JsValue> {
    let set_series = Reflect::get(chart, &"setSeries".into())?;
    let set_series_fn = Function::from(set_series);
    let opts = Object::new();
    Reflect::set(&opts, &"show".into(), &JsValue::from_bool(show))?;
    let args = Array::new();
    args.push(&JsValue::from_f64(series_idx as f64));
    args.push(&opts);
    Reflect::apply(&set_series_fn, chart, &args)?;
    Ok(())
}

fn create_resize_observer(
    chart: &JsValue,
    container: &web_sys::Element,
) -> (Option<ResizeObserver>, Option<ObserverClosure>) {
    let chart = chart.clone();
    let container_for_closure = container.clone();
    let closure = Closure::wrap(Box::new(move |entries: Vec<ResizeObserverEntry>| {
        for entry in entries {
            if entry.target() == container_for_closure {
                let rect = entry.content_rect();
                let width = rect.width().max(1.0);
                let height = rect.height().max(1.0);
                let _ = resize_chart(&chart, width, height);
            }
        }
    }) as Box<dyn FnMut(Vec<ResizeObserverEntry>)>);

    match ResizeObserver::new(closure.as_ref().unchecked_ref()) {
        Ok(observer) => {
            observer.observe(container);
            (Some(observer), Some(closure))
        }
        Err(err) => {
            web_sys::console::error_1(&format!("failed to create ResizeObserver: {err:?}").into());
            (None, Some(closure))
        }
    }
}

fn build_series_data<T: Clone>(
    data: &[T],
    x_extractor: ChartMetric<T>,
    series: &[ChartSeries<T>],
) -> Array {
    let xs = Array::new();
    let ys_list: Vec<Array> = series.iter().map(|_| Array::new()).collect();
    for point in data {
        xs.push(&JsValue::from_f64(x_extractor.extract(point)));
        for (i, s) in series.iter().enumerate() {
            {
                // uPlot gaps must be `null`, not NaN — NaN poisons the shared
                // y-scale range computation and nothing renders at all.
                let v = s.y_extractor.extract(point);
                if v.is_nan() {
                    ys_list[i].push(&JsValue::NULL);
                } else {
                    ys_list[i].push(&JsValue::from_f64(v));
                }
            }
        }
    }
    let series_data = Array::new();
    series_data.push(&xs);
    for ys in ys_list {
        series_data.push(&ys);
    }
    series_data
}

fn build_opts<T: Clone>(
    series: &[ChartSeries<T>],
    width: f64,
    height: f64,
    set_cursor: &Closure<dyn FnMut(JsValue)>,
) -> Object {
    let opts = Object::new();
    Reflect::set(&opts, &"width".into(), &JsValue::from_f64(width)).unwrap();
    Reflect::set(&opts, &"height".into(), &JsValue::from_f64(height)).unwrap();

    let series_arr = Array::new();
    series_arr.push(&Object::new()); // x-axis series placeholder
    for s in series {
        let y_series = Object::new();
        Reflect::set(&y_series, &"label".into(), &s.label.as_str().into()).unwrap();
        Reflect::set(&y_series, &"stroke".into(), &rgb_to_hex(s.color).into()).unwrap();
        Reflect::set(&y_series, &"width".into(), &JsValue::from_f64(2.0)).unwrap();
        if s.span_gaps {
            Reflect::set(&y_series, &"spanGaps".into(), &JsValue::TRUE).unwrap();
        }
        if let Some(ref dash) = s.dash {
            let dash_arr = Array::new();
            for v in dash {
                dash_arr.push(&JsValue::from_f64(*v));
            }
            Reflect::set(&y_series, &"dash".into(), &dash_arr).unwrap();
        }
        series_arr.push(&y_series);
    }
    Reflect::set(&opts, &"series".into(), &series_arr).unwrap();

    let axes = Array::new();
    axes.push(&x_axis_opts());
    axes.push(&axis_opts());
    Reflect::set(&opts, &"axes".into(), &axes).unwrap();

    let legend = Object::new();
    Reflect::set(&legend, &"show".into(), &JsValue::from_bool(false)).unwrap();
    Reflect::set(&opts, &"legend".into(), &legend).unwrap();

    let hooks = Object::new();
    let set_cursor_arr = Array::new();
    set_cursor_arr.push(set_cursor.as_ref());
    Reflect::set(&hooks, &"setCursor".into(), &set_cursor_arr).unwrap();
    Reflect::set(&opts, &"hooks".into(), &hooks).unwrap();

    let scales = Object::new();
    let x_scale = Object::new();
    Reflect::set(&x_scale, &"auto".into(), &JsValue::from_bool(true)).unwrap();
    Reflect::set(&x_scale, &"time".into(), &JsValue::from_bool(false)).unwrap();
    Reflect::set(&scales, &"x".into(), &x_scale).unwrap();
    let y_scale = Object::new();
    Reflect::set(&y_scale, &"auto".into(), &JsValue::from_bool(true)).unwrap();
    Reflect::set(&scales, &"y".into(), &y_scale).unwrap();
    Reflect::set(&opts, &"scales".into(), &scales).unwrap();

    opts
}

fn x_axis_opts() -> Object {
    let axis = axis_opts();
    Reflect::set(&axis, &"time".into(), &JsValue::from_bool(false)).unwrap();
    Reflect::set(&axis, &"label".into(), &"Time".into()).unwrap();

    let values_fn = Function::new_with_args(
        "_self, splits",
        r#"return splits.map(function(v) {
            if (v >= 60) return (v / 60).toFixed(1) + "m";
            return v.toFixed(0) + "s";
        });"#,
    );
    Reflect::set(&axis, &"values".into(), &values_fn).unwrap();

    axis
}

fn axis_opts() -> Object {
    let axis = Object::new();
    Reflect::set(&axis, &"stroke".into(), &"#a3a3a3".into()).unwrap();

    let grid = Object::new();
    Reflect::set(&grid, &"stroke".into(), &"#404040".into()).unwrap();
    Reflect::set(&grid, &"width".into(), &JsValue::from_f64(1.0)).unwrap();
    Reflect::set(&axis, &"grid".into(), &grid).unwrap();

    let ticks = Object::new();
    Reflect::set(&ticks, &"stroke".into(), &"#525252".into()).unwrap();
    Reflect::set(&ticks, &"width".into(), &JsValue::from_f64(1.0)).unwrap();
    Reflect::set(&axis, &"ticks".into(), &ticks).unwrap();

    axis
}

fn build_set_cursor_closure<T: Clone + 'static>(
    data: Signal<Vec<T>>,
    x_extractor: ChartMetric<T>,
    series: &[ChartSeries<T>],
    mut tooltip: TooltipSignals,
    visibility: Signal<Vec<Vec<bool>>>,
    tab_index: usize,
) -> Closure<dyn FnMut(JsValue)> {
    let series = series.to_vec();
    Closure::wrap(Box::new(move |u: JsValue| {
        let Ok(cursor) = Reflect::get(&u, &"cursor".into()) else {
            tooltip.content.set(None);
            return;
        };
        let Ok(idx_val) = Reflect::get(&cursor, &"idx".into()) else {
            tooltip.content.set(None);
            return;
        };
        let Some(idx) = idx_val.as_f64() else {
            tooltip.content.set(None);
            return;
        };
        if idx < 0.0 {
            tooltip.content.set(None);
            return;
        }

        let idx = idx as usize;
        let points = data.read();
        let Some(point) = points.get(idx) else {
            tooltip.content.set(None);
            return;
        };

        let x = x_extractor.extract(point);
        let values: Vec<(String, String)> = series
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                visibility
                    .read()
                    .get(tab_index)
                    .and_then(|v| v.get(*i).copied())
                    .unwrap_or(true)
            })
            .map(|(_, s)| {
                let y = s.y_extractor.extract(point);
                (s.label.clone(), format!("{:.2}", y))
            })
            .collect();
        if values.is_empty() {
            tooltip.content.set(None);
            return;
        }
        tooltip.content.set(Some((format_time(x), values)));

        if let (Some(left), Some(top)) = (
            Reflect::get(&cursor, &"left".into())
                .ok()
                .and_then(|v| v.as_f64()),
            Reflect::get(&cursor, &"top".into())
                .ok()
                .and_then(|v| v.as_f64()),
        ) {
            tooltip.pos.set((left, top));
        }
    }) as Box<dyn FnMut(JsValue)>)
}

fn format_time(seconds: f64) -> String {
    if seconds >= 60.0 {
        format!("{:.1}m", seconds / 60.0)
    } else {
        format!("{:.1}s", seconds)
    }
}

fn rgb_to_hex(color: plotters::prelude::RGBColor) -> String {
    format!("#{:02x}{:02x}{:02x}", color.0, color.1, color.2)
}

/// Which Y-axis a series should be plotted against in a dual-axis chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisSide {
    Left,
    Right,
}

/// Configuration for one series in a dual-axis chart.
#[derive(Clone, PartialEq)]
pub struct DualAxisSeries<T> {
    /// Label shown in the legend and tooltip.
    pub label: String,
    /// Color used for the line.
    pub color: RGBColor,
    /// Which Y-axis this series uses.
    pub side: AxisSide,
    /// Extracts the y value for a data point.
    pub y_extractor: ChartMetric<T>,
}

impl<T> DualAxisSeries<T> {
    pub fn new(
        label: impl Into<String>,
        color: RGBColor,
        side: AxisSide,
        y_extractor: ChartMetric<T>,
    ) -> Self {
        Self {
            label: label.into(),
            color,
            side,
            y_extractor,
        }
    }
}

/// High-performance dual-axis time-series chart backed by uPlot.
///
/// This is a specialized variant of [`UplotChart`] that always renders a single
/// chart with two Y-axes: one on the left and one on the right. Each series is
/// assigned to a side via [`AxisSide`].
#[component]
pub fn DualAxisUplotChart<T: Clone + PartialEq + 'static>(
    data: Signal<Vec<T>>,
    x_extractor: ChartMetric<T>,
    series: Vec<DualAxisSeries<T>>,
    #[props(default = "Left axis")] left_axis_label: &'static str,
    #[props(default = "Right axis")] right_axis_label: &'static str,
) -> Element {
    let chart_id = use_hook(|| {
        let id = CHART_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("uplot-dual-axis-chart-{id}")
    });
    let chart_state: Rc<RefCell<Option<ChartHandle>>> = use_hook(|| Rc::new(RefCell::new(None)));
    let chart_state_for_effect = chart_state.clone();
    let chart_state_for_cleanup = chart_state.clone();

    let tooltip: TooltipSignal = use_signal(|| None);
    let tooltip_pos = use_signal(|| (0.0_f64, 0.0_f64));

    use_drop(move || {
        if let Some(handle) = chart_state_for_cleanup.borrow_mut().take() {
            if let Some(observer) = handle.observer {
                observer.disconnect();
            }
            let _ = destroy_chart(&handle.chart);
        }
    });

    let chart_id_for_effect = chart_id.clone();
    let series_for_effect = series.clone();
    use_effect(move || {
        let points = data.read();
        let mut state = chart_state_for_effect.borrow_mut();
        let should_create = state.is_none();
        if should_create {
            if let Some(old_handle) = state.take() {
                if let Some(observer) = old_handle.observer {
                    observer.disconnect();
                }
                let _ = destroy_chart(&old_handle.chart);
            }
            if let Some(handle) = create_dual_axis_chart(
                &chart_id_for_effect,
                data,
                x_extractor,
                &series_for_effect,
                TooltipSignals {
                    content: tooltip,
                    pos: tooltip_pos,
                },
                left_axis_label,
                right_axis_label,
            ) {
                *state = Some(handle);
            }
        } else if let Some(handle) = state.as_ref() {
            let _ = update_dual_axis_chart(&handle.chart, &points, x_extractor, &series_for_effect);
        }
    });

    rsx! {
        document::Style {
            r#"
            .uplot-chart-container .uplot {{
                width: 100%;
                height: 100%;
            }}
            .uplot-chart-container .uplot .title {{
                color: #ffffff;
            }}
            .uplot-chart-container .uplot .u-axis {{
                color: #a3a3a3;
            }}
            "#
        }
        div { class: "flex-1 flex flex-col min-h-0",
            div { class: "flex items-center justify-center gap-4 mb-1 shrink-0",
                for s in series.iter() {
                    LegendItem {
                        color: rgb_to_hex(s.color),
                        label: s.label.clone(),
                    }
                }
            }
            div { class: "flex-1 rounded-lg border border-neutral-800 bg-[#171717] p-2 min-h-0 overflow-hidden flex flex-col",
                div { class: "flex-1 min-h-0 relative",
                    div {
                        id: "{chart_id}",
                        class: "absolute inset-0 uplot-chart-container",
                    }
                    if let Some((time, values)) = tooltip.read().as_ref() {
                        div {
                            class: "absolute z-10 px-2 py-1 rounded bg-neutral-900 border border-neutral-700 text-xs text-white shadow pointer-events-none",
                            style: "left: {tooltip_pos.read().0 + 12.0}px; top: {tooltip_pos.read().1 - 64.0}px;",
                            div { "{time}" }
                            for (label , value) in values.iter() {
                                div { "{label}: {value}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn create_dual_axis_chart<T: Clone + 'static>(
    element_id: &str,
    data: Signal<Vec<T>>,
    x_extractor: ChartMetric<T>,
    series: &[DualAxisSeries<T>],
    tooltip: TooltipSignals,
    left_axis_label: &'static str,
    right_axis_label: &'static str,
) -> Option<ChartHandle> {
    let window = window()?;
    let document = window.document()?;
    let container = document.get_element_by_id(element_id)?;

    let points = data.read();
    let series_data = build_dual_axis_series_data(&points, x_extractor, series);
    let width = container.client_width().max(1) as f64;
    let height = container.client_height().max(1) as f64;

    let set_cursor = build_dual_axis_set_cursor_closure(data, x_extractor, series, tooltip);
    let opts = build_dual_axis_opts(
        series,
        width,
        height,
        &set_cursor,
        left_axis_label,
        right_axis_label,
    );

    let uplot = Reflect::get(&window, &"uPlot".into()).ok()?;
    let uplot_fn = Function::from(uplot);
    let args = Array::new();
    args.push(&opts);
    args.push(&series_data);
    args.push(&container);
    let chart = Reflect::construct(&uplot_fn, &args).ok()?;

    let (observer, _observer_closure) = create_resize_observer(&chart, &container);

    let hooks = vec![set_cursor];
    Some(ChartHandle {
        index: 0,
        chart,
        hooks,
        observer,
        _observer_closure,
    })
}

fn update_dual_axis_chart<T: Clone>(
    chart: &JsValue,
    data: &[T],
    x_extractor: ChartMetric<T>,
    series: &[DualAxisSeries<T>],
) -> Result<(), JsValue> {
    let series_data = build_dual_axis_series_data(data, x_extractor, series);
    let set_data = Reflect::get(chart, &"setData".into())?;
    let set_data_fn = Function::from(set_data);
    Reflect::apply(&set_data_fn, chart, &Array::of1(&series_data))?;
    Ok(())
}

fn build_dual_axis_series_data<T: Clone>(
    data: &[T],
    x_extractor: ChartMetric<T>,
    series: &[DualAxisSeries<T>],
) -> Array {
    let xs = Array::new();
    let ys_list: Vec<Array> = series.iter().map(|_| Array::new()).collect();
    for point in data {
        xs.push(&JsValue::from_f64(x_extractor.extract(point)));
        for (i, s) in series.iter().enumerate() {
            {
                // uPlot gaps must be `null`, not NaN — NaN poisons the shared
                // y-scale range computation and nothing renders at all.
                let v = s.y_extractor.extract(point);
                if v.is_nan() {
                    ys_list[i].push(&JsValue::NULL);
                } else {
                    ys_list[i].push(&JsValue::from_f64(v));
                }
            }
        }
    }
    let series_data = Array::new();
    series_data.push(&xs);
    for ys in ys_list {
        series_data.push(&ys);
    }
    series_data
}

fn build_dual_axis_opts<T: Clone>(
    series: &[DualAxisSeries<T>],
    width: f64,
    height: f64,
    set_cursor: &Closure<dyn FnMut(JsValue)>,
    left_axis_label: &'static str,
    right_axis_label: &'static str,
) -> Object {
    let opts = Object::new();
    Reflect::set(&opts, &"width".into(), &JsValue::from_f64(width)).unwrap();
    Reflect::set(&opts, &"height".into(), &JsValue::from_f64(height)).unwrap();

    let series_arr = Array::new();
    series_arr.push(&Object::new()); // x-axis series placeholder
    for s in series {
        let y_series = Object::new();
        Reflect::set(&y_series, &"label".into(), &s.label.as_str().into()).unwrap();
        Reflect::set(&y_series, &"stroke".into(), &rgb_to_hex(s.color).into()).unwrap();
        Reflect::set(&y_series, &"width".into(), &JsValue::from_f64(2.0)).unwrap();
        let scale_name = match s.side {
            AxisSide::Left => "left",
            AxisSide::Right => "right",
        };
        Reflect::set(&y_series, &"scale".into(), &scale_name.into()).unwrap();
        series_arr.push(&y_series);
    }
    Reflect::set(&opts, &"series".into(), &series_arr).unwrap();

    let axes = Array::new();
    axes.push(&x_axis_opts());

    let left_axis = axis_opts();
    Reflect::set(&left_axis, &"scale".into(), &"left".into()).unwrap();
    Reflect::set(&left_axis, &"label".into(), &left_axis_label.into()).unwrap();
    axes.push(&left_axis);

    let right_axis = axis_opts();
    Reflect::set(&right_axis, &"scale".into(), &"right".into()).unwrap();
    Reflect::set(&right_axis, &"side".into(), &JsValue::from_f64(1.0)).unwrap();
    Reflect::set(&right_axis, &"label".into(), &right_axis_label.into()).unwrap();
    axes.push(&right_axis);

    Reflect::set(&opts, &"axes".into(), &axes).unwrap();

    let hooks = Object::new();
    let set_cursor_arr = Array::new();
    set_cursor_arr.push(set_cursor.as_ref());
    Reflect::set(&hooks, &"setCursor".into(), &set_cursor_arr).unwrap();
    Reflect::set(&opts, &"hooks".into(), &hooks).unwrap();

    let scales = Object::new();
    let x_scale = Object::new();
    Reflect::set(&x_scale, &"auto".into(), &JsValue::from_bool(true)).unwrap();
    Reflect::set(&x_scale, &"time".into(), &JsValue::from_bool(false)).unwrap();
    Reflect::set(&scales, &"x".into(), &x_scale).unwrap();

    let left_scale = Object::new();
    Reflect::set(&left_scale, &"auto".into(), &JsValue::from_bool(true)).unwrap();
    Reflect::set(&scales, &"left".into(), &left_scale).unwrap();

    let right_scale = Object::new();
    Reflect::set(&right_scale, &"auto".into(), &JsValue::from_bool(true)).unwrap();
    Reflect::set(&scales, &"right".into(), &right_scale).unwrap();

    Reflect::set(&opts, &"scales".into(), &scales).unwrap();

    opts
}

fn build_dual_axis_set_cursor_closure<T: Clone + 'static>(
    data: Signal<Vec<T>>,
    x_extractor: ChartMetric<T>,
    series: &[DualAxisSeries<T>],
    mut tooltip: TooltipSignals,
) -> Closure<dyn FnMut(JsValue)> {
    let series = series.to_vec();
    Closure::wrap(Box::new(move |u: JsValue| {
        let Ok(cursor) = Reflect::get(&u, &"cursor".into()) else {
            tooltip.content.set(None);
            return;
        };
        let Ok(idx_val) = Reflect::get(&cursor, &"idx".into()) else {
            tooltip.content.set(None);
            return;
        };
        let Some(idx) = idx_val.as_f64() else {
            tooltip.content.set(None);
            return;
        };
        if idx < 0.0 {
            tooltip.content.set(None);
            return;
        }

        let idx = idx as usize;
        let points = data.read();
        let Some(point) = points.get(idx) else {
            tooltip.content.set(None);
            return;
        };

        let x = x_extractor.extract(point);
        let values = series
            .iter()
            .map(|s| {
                let y = s.y_extractor.extract(point);
                (s.label.clone(), format!("{:.2}", y))
            })
            .collect();
        tooltip.content.set(Some((format_time(x), values)));

        if let (Some(left), Some(top)) = (
            Reflect::get(&cursor, &"left".into())
                .ok()
                .and_then(|v| v.as_f64()),
            Reflect::get(&cursor, &"top".into())
                .ok()
                .and_then(|v| v.as_f64()),
        ) {
            tooltip.pos.set((left, top));
        }
    }) as Box<dyn FnMut(JsValue)>)
}
