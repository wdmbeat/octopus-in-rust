use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_core::control::{ControlRequest, ControlResponse};
use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tracing::{error, info, warn};

use crate::group_brain::GroupBrainManager;

/// Resolve the control socket path from the core config file path.
///
/// `config_path` is expected to be `<data_dir>/config.toml`, so the socket is
/// placed at `<data_dir>/../run/qqbot-core.sock` to match the layout used by
/// the `qqbot` supervisor for the pid file.
pub fn socket_path(config_path: &Path) -> Option<PathBuf> {
    let data_dir = config_path.parent()?;
    let base_dir = data_dir.parent().unwrap_or_else(|| Path::new("."));
    let run_dir = base_dir.join("run");
    Some(run_dir.join("qqbot-core.sock"))
}

/// Start the local control socket server.
///
/// On non-Unix platforms this is a no-op. The server accepts one JSON request
/// per connection, dispatches it, and writes back a JSON response.
pub async fn serve(config_path: PathBuf, manager: Arc<GroupBrainManager>) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (config_path, manager);
        return Ok(());
    }

    #[cfg(unix)]
    {
        let socket_path =
            socket_path(&config_path).context("could not resolve control socket path")?;
        let run_dir = socket_path
            .parent()
            .context("socket path has no parent directory")?;
        std::fs::create_dir_all(run_dir)?;

        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }

        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("failed to bind control socket {}", socket_path.display()))?;
        info!(path = %socket_path.display(), "control socket listening");

        loop {
            match listener.accept().await {
                Ok((mut stream, _addr)) => {
                    let manager = manager.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        match stream.read(&mut buf).await {
                            Ok(0) => (),
                            Ok(n) => {
                                let request =
                                    match serde_json::from_slice::<ControlRequest>(&buf[..n]) {
                                        Ok(req) => req,
                                        Err(e) => {
                                            let resp = ControlResponse::Error {
                                                message: format!("invalid request: {e}"),
                                            };
                                            let _ = write_response(&mut stream, &resp).await;
                                            return;
                                        }
                                    };
                                let response = handle_request(request, manager).await;
                                let _ = write_response(&mut stream, &response).await;
                            }
                            Err(e) => {
                                warn!(error = %e, "control socket read failed");
                            }
                        }
                    });
                }
                Err(e) => {
                    error!(error = %e, "control socket accept failed");
                }
            }
        }
    }
}

#[cfg(unix)]
async fn handle_request(
    request: ControlRequest,
    manager: Arc<GroupBrainManager>,
) -> ControlResponse {
    match request {
        ControlRequest::ListTools => {
            let tools = manager.loaded_tools().await;
            ControlResponse::Tools { tools }
        }
        ControlRequest::GroupStatus => {
            let groups = manager.group_status().await;
            ControlResponse::Groups { groups }
        }
        ControlRequest::Ping => ControlResponse::Pong,
    }
}

#[cfg(unix)]
async fn write_response(
    stream: &mut tokio::net::UnixStream,
    response: &ControlResponse,
) -> Result<()> {
    let bytes = serde_json::to_vec(response).context("failed to serialize control response")?;
    stream
        .write_all(&bytes)
        .await
        .context("failed to write control response")?;
    Ok(())
}
