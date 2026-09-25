//! Shared error type and Axum response mapping.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::fmt;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Requested resource was not found.
    NotFound,

    /// Configuration value missing or invalid.
    Config(String),

    /// I/O failure.
    Io(std::io::Error),

    /// Blueprint lookup or parse failure.
    Blueprint(Box<faf_blueprints::Error>),

    /// Agent (Q&A) turn failed.
    Agent(Box<agent_core::BrainError>),

    /// Missing or invalid upload credential.
    Unauthorized,

    /// Client sent an invalid request.
    BadRequest(String),

    /// Request conflicts with current server state.
    Conflict(String),

    /// Feature is not configured on this server.
    Unavailable(String),

    /// Catch-all for unexpected internal failures.
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::Config(msg) => write!(f, "config error: {msg}"),
            Error::Io(err) => write!(f, "io error: {err}"),
            Error::Blueprint(err) => write!(f, "{err}"),
            Error::Agent(err) => write!(f, "{err}"),
            Error::Unauthorized => write!(f, "unauthorized"),
            Error::BadRequest(msg) => write!(f, "bad request: {msg}"),
            Error::Conflict(msg) => write!(f, "conflict: {msg}"),
            Error::Unavailable(msg) => write!(f, "unavailable: {msg}"),
            Error::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(err: std::num::ParseIntError) -> Self {
        Error::Config(err.to_string())
    }
}

impl From<faf_blueprints::Error> for Error {
    fn from(err: faf_blueprints::Error) -> Self {
        Error::Blueprint(Box::new(err))
    }
}

impl From<agent_core::BrainError> for Error {
    fn from(err: agent_core::BrainError) -> Self {
        Error::Agent(Box::new(err))
    }
}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Error::Internal(err.to_string())
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Error::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
            Error::Config(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("config error: {msg}"),
            )
                .into_response(),
            Error::Io(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("io error: {err}"),
            )
                .into_response(),
            Error::Blueprint(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("{err}")).into_response()
            }
            Error::Agent(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("{err}")).into_response()
            }
            Error::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
            Error::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, format!("bad request: {msg}")).into_response()
            }
            Error::Conflict(msg) => {
                (StatusCode::CONFLICT, format!("conflict: {msg}")).into_response()
            }
            Error::Unavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("unavailable: {msg}"),
            )
                .into_response(),
            Error::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("internal error: {msg}"),
            )
                .into_response(),
        }
    }
}
