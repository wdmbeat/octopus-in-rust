use crate::exception::{OctopusError, Result};
use crate::share::get_share_dir;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ============================================================================
// Constants
// ============================================================================

pub const KIMI_CODE_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
pub const KIMI_CODE_OAUTH_KEY: &str = "oauth/kimi-code";
pub const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
pub const REFRESH_INTERVAL_SECONDS: u64 = 60;
pub const MIN_REFRESH_THRESHOLD_SECONDS: f64 = 300.0;
pub const REFRESH_THRESHOLD_RATIO: f64 = 0.5;
pub const UNAUTHORIZED_REFRESH_RETRY_COOLDOWN_SECONDS: u64 = 300;
pub const MAX_REFRESH_RETRIES: usize = 3;

// ============================================================================
// Data structures
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Absolute Unix timestamp when the token expires.
    #[serde(default)]
    pub expires_at: f64,
    #[serde(default)]
    pub scope: String,
    #[serde(default = "default_bearer")]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: f64,
}

fn default_bearer() -> String {
    "Bearer".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorization {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: Option<u64>,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

// ============================================================================
// Token storage
// ============================================================================

fn credentials_dir() -> PathBuf {
    let path = get_share_dir().join("credentials");
    if !path.exists() {
        let _ = std::fs::create_dir_all(&path);
    }
    path
}

pub fn credentials_path(key: &str) -> PathBuf {
    let name = key.strip_prefix("oauth/").unwrap_or(key);
    let name = name.split('/').next_back().unwrap_or(name);
    credentials_dir().join(format!("{}.json", name))
}

pub fn load_tokens(key: &str) -> Option<OAuthToken> {
    let path = credentials_path(key);
    if !path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_tokens(key: &str, token: &OAuthToken) -> Result<()> {
    let path = credentials_path(key);
    let parent = path
        .parent()
        .ok_or_else(|| OctopusError::Other("Invalid credentials path".to_string()))?;

    let text = serde_json::to_string_pretty(token)
        .map_err(|e| OctopusError::Other(format!("Failed to serialize token: {}", e)))?;

    // Atomic write: write to temp file, fsync, then rename
    let temp_path = parent.join(format!(".tmp-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&temp_path, text).map_err(OctopusError::Io)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&temp_path)
            .map_err(OctopusError::Io)?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&temp_path, perms).map_err(OctopusError::Io)?;
    }

    std::fs::rename(&temp_path, &path).map_err(OctopusError::Io)?;

    Ok(())
}

pub fn delete_tokens(key: &str) -> Result<()> {
    let path = credentials_path(key);
    if path.exists() {
        std::fs::remove_file(&path).map_err(OctopusError::Io)?;
    }
    Ok(())
}

// ============================================================================
// Device flow
// ============================================================================

pub async fn request_device_authorization() -> Result<DeviceAuthorization> {
    let oauth_host = crate::auth::platforms::oauth_host();
    let url = format!(
        "{}/api/oauth/device_authorization",
        oauth_host.trim_end_matches('/')
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .form(&[("client_id", KIMI_CODE_CLIENT_ID)])
        .send()
        .await
        .map_err(|e| OctopusError::Other(format!("Device authorization request failed: {}", e)))?;

    let status = response.status();
    let body: DeviceAuthorization = response.json().await.map_err(|e| {
        OctopusError::Other(format!(
            "Failed to parse device authorization response: {}",
            e
        ))
    })?;

    if !status.is_success() {
        return Err(OctopusError::Other(format!(
            "Device authorization failed ({}): {:?}",
            status, body
        )));
    }

    Ok(body)
}

pub async fn poll_device_token(
    device_code: &str,
    interval: u64,
    expires_in: Option<u64>,
) -> Result<OAuthToken> {
    let oauth_host = crate::auth::platforms::oauth_host();
    let url = format!("{}/api/oauth/token", oauth_host.trim_end_matches('/'));
    let client = reqwest::Client::new();

    let start = Instant::now();
    let max_duration = expires_in.map(Duration::from_secs);

    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;

        if let Some(max) = max_duration
            && start.elapsed() > max
        {
            return Err(OctopusError::Other(
                "Device authorization expired. Please try again.".to_string(),
            ));
        }

        let response = client
            .post(&url)
            .form(&[
                ("client_id", KIMI_CODE_CLIENT_ID),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
            ])
            .send()
            .await
            .map_err(|e| OctopusError::Other(format!("Token poll request failed: {}", e)))?;

        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| OctopusError::Other(format!("Failed to parse token response: {}", e)))?;

        if status.is_success() {
            return parse_token_response(&body);
        }

        // Check for "authorization_pending" error
        if let Some(error) = body["error"].as_str() {
            if error == "authorization_pending" {
                continue;
            }
            if error == "expired_token" {
                return Err(OctopusError::Other(
                    "Device code expired. Please try again.".to_string(),
                ));
            }
            if error == "access_denied" {
                return Err(OctopusError::Other("Access denied by user.".to_string()));
            }
        }

        return Err(OctopusError::Other(format!(
            "Token poll failed ({}): {:?}",
            status, body
        )));
    }
}

pub async fn login_kimi_code() -> Result<OAuthToken> {
    let device_auth = request_device_authorization().await?;

    println!("\nPlease visit: {}", device_auth.verification_uri_complete);
    println!("Or enter this code: {}\n", device_auth.user_code);

    // Try to open browser automatically
    let _ = webbrowser::open(&device_auth.verification_uri_complete);

    let token = poll_device_token(
        &device_auth.device_code,
        device_auth.interval,
        device_auth.expires_in,
    )
    .await?;

    save_tokens(KIMI_CODE_OAUTH_KEY, &token)?;
    Ok(token)
}

// ============================================================================
// Token refresh
// ============================================================================

pub async fn refresh_token(refresh_token_str: &str) -> Result<OAuthToken> {
    let oauth_host = crate::auth::platforms::oauth_host();
    let url = format!("{}/api/oauth/token", oauth_host.trim_end_matches('/'));
    let client = reqwest::Client::new();

    for attempt in 0..MAX_REFRESH_RETRIES {
        if attempt > 0 {
            let delay = Duration::from_secs(2u64.pow(attempt as u32));
            tokio::time::sleep(delay).await;
        }

        let response = match client
            .post(&url)
            .form(&[
                ("client_id", KIMI_CODE_CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token_str),
            ])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                if attempt == MAX_REFRESH_RETRIES - 1 {
                    return Err(OctopusError::Other(format!(
                        "Token refresh failed after {} retries: {}",
                        MAX_REFRESH_RETRIES, e
                    )));
                }
                continue;
            }
        };

        let status = response.status();
        let body: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                if attempt == MAX_REFRESH_RETRIES - 1 {
                    return Err(OctopusError::Other(format!(
                        "Token refresh failed after {} retries: {}",
                        MAX_REFRESH_RETRIES, e
                    )));
                }
                continue;
            }
        };

        if status.is_success() {
            return parse_token_response(&body);
        }

        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(OctopusError::Other(
                "OAuth refresh token rejected (401/403). Please log in again.".to_string(),
            ));
        }

        // Retryable status codes
        let retryable = [429u16, 500, 502, 503, 504];
        if !retryable.contains(&status.as_u16()) {
            return Err(OctopusError::Other(format!(
                "Token refresh failed ({}): {:?}",
                status, body
            )));
        }

        if attempt == MAX_REFRESH_RETRIES - 1 {
            return Err(OctopusError::Other(format!(
                "Token refresh failed after {} retries: retryable status {}",
                MAX_REFRESH_RETRIES, status
            )));
        }
    }

    unreachable!()
}

// ============================================================================
// Helpers
// ============================================================================

fn parse_token_response(body: &serde_json::Value) -> Result<OAuthToken> {
    let mut token: OAuthToken = serde_json::from_value(body.clone())
        .map_err(|e| OctopusError::Other(format!("Invalid token response: {}", e)))?;

    if token.expires_in > 0.0 && token.expires_at == 0.0 {
        token.expires_at = now_secs() + token.expires_in;
    }

    Ok(token)
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn refresh_threshold(expires_in: f64) -> f64 {
    if expires_in > 0.0 {
        MIN_REFRESH_THRESHOLD_SECONDS.max(expires_in * REFRESH_THRESHOLD_RATIO)
    } else {
        MIN_REFRESH_THRESHOLD_SECONDS
    }
}
