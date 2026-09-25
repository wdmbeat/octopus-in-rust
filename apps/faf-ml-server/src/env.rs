//! Environment-variable loading helpers.
//!
//! Loads a `.env` file (if present) at startup and provides typed helpers for
//! reading environment variables consistently across the server.

use std::path::PathBuf;

use crate::error::{Error, Result};

/// Load variables from `.env`.
///
/// First tries `apps/faf-ml-server/.env` (the crate manifest directory), then
/// falls back to searching the current working directory and its parents.
pub fn load() {
    let manifest_env = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env");
    if manifest_env.is_file() {
        let _ = dotenvy::from_path(&manifest_env);
        return;
    }
    let _ = dotenvy::dotenv();
}

/// Read an environment variable with a fallback default.
#[allow(dead_code)]
pub fn var_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Read a path environment variable, falling back to a default path.
pub fn path_or(key: &str, default: impl Into<PathBuf>) -> PathBuf {
    std::env::var(key)
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| default.into())
}

/// Parse an environment variable into a `u16` port.
pub fn port_or(key: &str, default: u16) -> Result<u16> {
    match std::env::var(key) {
        Ok(raw) => raw
            .parse::<u16>()
            .map_err(|e| Error::Config(format!("failed to parse {key}: {e}"))),
        Err(_) => Ok(default),
    }
}
