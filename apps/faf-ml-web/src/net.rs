//! Base-URL resolution for `faf-ml-server` API calls.
//!
//! Dev builds (`dx serve`, debug assertions) talk to a locally running server
//! on `localhost:3100`. Release builds derive the origin from
//! `window.location`, so the deployed site works on any host, port, and scheme
//! (the server serves this SPA itself, so same-origin is always correct).

/// Absolute URL for an HTTP API path like `/api/screenshots`.
pub fn api_url(path: &str) -> String {
    format!("{}{}", api_base(), path)
}

/// Absolute URL for a WebSocket path like `/ws/training`.
pub fn ws_url(path: &str) -> String {
    let base = api_base();
    let ws_base = match base.strip_prefix("https") {
        Some(rest) => format!("wss{rest}"),
        None => base.replacen("http", "ws", 1),
    };
    format!("{ws_base}{path}")
}

/// Absolute URL of a screenshot's PNG image.
pub fn image_url(id: &str) -> String {
    api_url(&format!("/api/screenshots/{id}/image"))
}

/// Absolute URL of a unit portrait image.
pub fn portrait_url(unit_id: &str) -> String {
    api_url(&format!("/api/portraits/{}", unit_id.to_ascii_uppercase()))
}

fn api_base() -> String {
    if cfg!(debug_assertions) {
        "http://localhost:3100".to_string()
    } else {
        web_sys::window()
            .and_then(|w| w.location().origin().ok())
            .unwrap_or_else(|| "http://localhost:3100".to_string())
    }
}
