use crate::core_config::CoreConfigFile;
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::path::Path;
use tokio::time::{timeout, Duration};
use tracing::{error, warn};

use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message},
    MaybeTlsStream, WebSocketStream,
};

#[derive(Debug, Clone)]
pub struct HealthReport {
    pub online: bool,
    pub bot_user_id: Option<i64>,
    pub bot_nickname: Option<String>,
    pub group_membership: Vec<GroupMembership>,
    pub echo: Option<GroupEcho>,
}

#[derive(Debug, Clone)]
pub struct GroupMembership {
    pub group_id: i64,
    pub member: bool,
    pub role: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GroupEcho {
    pub group_id: i64,
    pub received: bool,
    pub features: Vec<String>,
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
}

pub async fn run(data_dir: &Path, group_id: Option<i64>) -> Result<()> {
    let config = CoreConfigFile::from_file(data_dir.join("config.toml"))?;

    println!("=== qqbot health ===\n");

    match check(data_dir, &config, true, group_id).await {
        Ok(report) => print_report(&report),
        Err(e) => {
            error!(error = %e, "health check failed");
            println!("[fail] health check failed: {e}");
        }
    }

    Ok(())
}

pub async fn check(
    data_dir: &Path,
    config: &CoreConfigFile,
    send_echo: bool,
    group_id: Option<i64>,
) -> Result<HealthReport> {
    let mut ws = connect(config).await?;

    // 1. Get login info (also verifies the bot is logged in).
    let login = send_action(&mut ws, "get_login_info", Value::Object(Default::default())).await?;
    let bot_user_id = login
        .get("data")
        .and_then(|d| d.get("user_id"))
        .and_then(|v| v.as_i64());
    let bot_nickname = login
        .get("data")
        .and_then(|d| d.get("nickname"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 2. Get status to confirm online.
    let status = send_action(&mut ws, "get_status", Value::Object(Default::default())).await?;
    let online = status
        .get("data")
        .and_then(|d| d.get("online"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 3. Check membership in allowed groups.
    let mut group_membership = Vec::new();
    if let Some(uid) = bot_user_id {
        for gid in &config.bot.allowed_groups {
            let params = serde_json::json!({
                "group_id": gid,
                "user_id": uid,
                "no_cache": true,
            });
            let res = send_action(&mut ws, "get_group_member_info", params).await;
            match res {
                Ok(payload) => {
                    let data = payload.get("data");
                    let is_member = data.is_some();
                    let role = data
                        .and_then(|d| d.get("role"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    group_membership.push(GroupMembership {
                        group_id: *gid,
                        member: is_member,
                        role,
                    });
                }
                Err(e) => {
                    warn!(group_id = gid, error = %e, "failed to query group membership");
                    group_membership.push(GroupMembership {
                        group_id: *gid,
                        member: false,
                        role: None,
                    });
                }
            }
        }
    }

    // 4. End-to-end echo: send a message to the selected allowed group and
    //    confirm it appears in group history.
    let features: Vec<String> = crate::plugins::enabled_plugins(data_dir)
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut echo: Option<GroupEcho> = None;
    if send_echo && online && group_membership.iter().any(|g| g.member) {
        if let Some(bot_user_id) = bot_user_id {
            let group_id = match group_id {
                Some(id) => {
                    if !config.bot.allowed_groups.contains(&id) {
                        anyhow::bail!(
                            "group {id} is not in bot.allowed_groups; add it to config.toml first"
                        );
                    }
                    let gm = group_membership
                        .iter()
                        .find(|g| g.group_id == id)
                        .with_context(|| {
                            format!("group {id} membership could not be determined")
                        })?;
                    if !gm.member {
                        anyhow::bail!(
                            "bot is not a member of group {id}; add the bot to this group first"
                        );
                    }
                    id
                }
                None => group_membership
                    .iter()
                    .find(|g| g.member)
                    .map(|g| g.group_id)
                    .context("bot is not a member of any allowed group")?,
            };
            match check_group_echo(data_dir, config, group_id, bot_user_id).await {
                Ok(e) => echo = Some(e),
                Err(e) => {
                    warn!(error = %e, "group echo check failed");
                    echo = Some(GroupEcho {
                        group_id,
                        received: false,
                        features,
                        llm_provider: Some(config.llm.api_url().to_string()),
                        llm_model: Some(config.llm.model.clone()),
                    });
                }
            }
        }
    }

    let _ = ws.close(None).await;

    Ok(HealthReport {
        online,
        bot_user_id,
        bot_nickname,
        group_membership,
        echo,
    })
}

async fn connect(
    config: &CoreConfigFile,
) -> Result<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>> {
    use tokio_tungstenite::tungstenite::Error as WsError;

    let ws_url = &config.onebot.ws_url;
    let token = &config.onebot.access_token;

    let res = if token.is_empty() {
        connect_async(ws_url).await
    } else {
        let req = http::Request::builder()
            .uri(ws_url)
            .header("Authorization", format!("Bearer {token}"))
            .body(())?;
        connect_async(req).await
    };

    let (ws, _) = match res {
        Ok(pair) => pair,
        Err(WsError::Io(ref e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            anyhow::bail!("OneBot WebSocket TCP connection refused (is SnowLuma running?)")
        }
        Err(WsError::Io(ref e)) => {
            anyhow::bail!("OneBot WebSocket TCP error: {e}")
        }
        Err(WsError::Http(ref resp)) => {
            let code = resp.status();
            anyhow::bail!(
                "OneBot WebSocket handshake failed with HTTP {code} (QQ may not be logged in yet)"
            )
        }
        Err(e) => anyhow::bail!("OneBot WebSocket handshake failed: {e}"),
    };
    Ok(ws)
}

async fn check_group_echo(
    data_dir: &Path,
    config: &CoreConfigFile,
    group_id: i64,
    _bot_user_id: i64,
) -> Result<GroupEcho> {
    let mut ws = connect(config).await?;

    let token = uuid::Uuid::new_v4().to_string();
    let features: Vec<String> = match crate::plugins::enabled_plugins(data_dir) {
        Ok(names) => names.into_iter().collect(),
        Err(_) => Vec::new(),
    };
    let available_count = crate::plugins::available_plugins()
        .unwrap_or_default()
        .len();
    let enabled_count = features.len();

    let mut lines = vec!["LLM:".to_string()];
    lines.push(format!("  provider: {}", config.llm.api_url()));
    lines.push(format!("  model: {}", config.llm.model));

    lines.push("Tools loaded:".to_string());
    if features.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        for (i, name) in features.iter().enumerate() {
            let desc = crate::plugins::plugin_description(name);
            lines.push(format!("  {}. {} -- {}", i + 1, name, desc));
        }
    }
    lines.push(format!(
        "Total: {enabled_count}/{available_count} tools available"
    ));

    lines.push(crate::plugins::help_reminder().to_string());
    lines.push(format!("check id: {token}"));
    let text = lines.join("\n");
    let params = serde_json::json!({
        "group_id": group_id,
        "message": text,
    });

    // 1. Send a test message to the group.
    let send_resp = send_action(&mut ws, "send_group_msg", params).await?;
    let message_id = send_resp
        .get("data")
        .and_then(|d| d.get("message_id"))
        .cloned();

    // 2. Confirm the message appears in recent group history (retry briefly).
    let history_params = serde_json::json!({
        "group_id": group_id,
        "count": 20,
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while tokio::time::Instant::now() < deadline && !found {
        match send_action(&mut ws, "get_group_msg_history", history_params.clone()).await {
            Ok(history) => {
                found = history
                    .get("data")
                    .and_then(|d| d.get("messages"))
                    .and_then(|m| m.as_array())
                    .map(|messages| {
                        messages.iter().any(|msg| {
                            let id_matches = message_id
                                .as_ref()
                                .zip(msg.get("message_id"))
                                .map(|(sent, got)| sent == got)
                                .unwrap_or(false);
                            let text_matches = msg
                                .get("raw_message")
                                .and_then(|v| v.as_str())
                                .map(|s| s.contains(&token))
                                .unwrap_or(false);
                            id_matches || text_matches
                        })
                    })
                    .unwrap_or(false);
            }
            Err(e) => {
                warn!(error = %e, "get_group_msg_history failed");
            }
        }
        if !found {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    let _ = ws.close(None).await;

    Ok(GroupEcho {
        group_id,
        received: found,
        features,
        llm_provider: Some(config.llm.api_url().to_string()),
        llm_model: Some(config.llm.model.clone()),
    })
}

async fn send_action(
    ws: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    action: &str,
    params: Value,
) -> Result<Value> {
    let echo = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "action": action,
        "params": params,
        "echo": echo,
    });

    ws.send(Message::Text(payload.to_string()))
        .await
        .map_err(|e: WsError| anyhow::anyhow!("failed to send action to OneBot: {e}"))?;

    let deadline = Duration::from_secs(5);
    let response = timeout(deadline, async {
        while let Some(msg) = ws.next().await {
            let msg = msg.map_err(|e: WsError| {
                anyhow::anyhow!("WebSocket error while waiting for response: {e}")
            })?;
            if let Message::Text(text) = msg {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    if value.get("echo").and_then(|e| e.as_str()) == Some(&echo) {
                        return Ok::<_, anyhow::Error>(value);
                    }
                }
            }
        }
        anyhow::bail!("WebSocket closed before response received")
    })
    .await
    .context("timed out waiting for OneBot response")??;

    Ok(response)
}

fn print_report(report: &HealthReport) {
    if report.online {
        println!("[ok] QQ account is online");
    } else {
        println!("[fail] QQ account is not online");
    }

    if let (Some(uid), Some(nick)) = (report.bot_user_id, report.bot_nickname.clone()) {
        println!("[ok] Logged in as {nick} ({uid})");
    } else if let Some(uid) = report.bot_user_id {
        println!("[ok] Logged in with user id {uid}");
    } else {
        println!("[fail] Could not determine bot user id");
    }

    if report.group_membership.is_empty() {
        println!("[warn] No allowed groups configured");
    } else {
        for gm in &report.group_membership {
            if gm.member {
                let role = gm.role.as_deref().unwrap_or("member");
                println!(
                    "[ok] Bot is a member of allowed group {} (role: {})",
                    gm.group_id, role
                );
            } else {
                println!(
                    "[fail] Bot is NOT a member of allowed group {} — add the bot to this group first",
                    gm.group_id
                );
            }
        }
    }

    if let Some(ref echo) = report.echo {
        if echo.received {
            println!(
                "[ok] End-to-end check: sent a test message and confirmed it in group {}",
                echo.group_id
            );
            println!(
                "       LLM provider: {}",
                echo.llm_provider.as_deref().unwrap_or("unknown")
            );
            println!(
                "       LLM model: {}",
                echo.llm_model.as_deref().unwrap_or("unknown")
            );
            println!("       Tools loaded:");
            if echo.features.is_empty() {
                println!("         (none)");
            } else {
                for (i, name) in echo.features.iter().enumerate() {
                    println!(
                        "         {}. {} -- {}",
                        i + 1,
                        name,
                        crate::plugins::plugin_description(name)
                    );
                }
            }
            println!("       {}", crate::plugins::help_reminder());
        } else {
            println!(
                "[warn] Sent a test message to group {} but could not confirm it in recent history",
                echo.group_id
            );
        }
    }

    println!();
    let echo_ok = report.echo.as_ref().map(|e| e.received).unwrap_or(false);
    if report.online
        && report.bot_user_id.is_some()
        && report.group_membership.iter().all(|g| g.member)
        && echo_ok
    {
        println!("Health: bot is ready to send and receive group messages.");
    } else if report.online
        && report.bot_user_id.is_some()
        && report.group_membership.iter().all(|g| g.member)
    {
        println!("Health: bot can reach QQ and is in the allowed group(s), but the end-to-end echo check did not complete.");
    } else {
        println!("Health: bot is not ready to send/receive messages.");
        println!("Run `qqbot logs core -n 50` for details.");
    }
}
