//! OAuth token management for subscription-based LLM providers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{error, info};

/// Minimum seconds before expiry at which a token is considered in need of
/// refresh.
const MIN_REFRESH_THRESHOLD_SECONDS: f64 = 300.0;

/// Configuration required to refresh an OAuth access token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthConfig {
    /// OAuth client id.
    pub client_id: String,
    /// Token endpoint URL.
    pub token_endpoint: String,
}

impl OAuthConfig {
    /// Kimi Code managed endpoint defaults.
    pub fn kimi_code() -> Self {
        Self {
            client_id: "17e5f671-d194-4dfb-9706-5516cb48c098".to_string(),
            token_endpoint: "https://auth.kimi.com/api/oauth/token".to_string(),
        }
    }
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self::kimi_code()
    }
}

/// Persisted OAuth token.
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

/// Manages reading and refreshing an OAuth access token stored on disk.
#[derive(Clone)]
pub struct OAuthManager {
    config: OAuthConfig,
    token_file: PathBuf,
    cached: Arc<Mutex<Option<OAuthToken>>>,
}

impl OAuthManager {
    pub fn new(config: OAuthConfig, token_file: impl Into<PathBuf>) -> Self {
        Self {
            config,
            token_file: token_file.into(),
            cached: Arc::new(Mutex::new(None)),
        }
    }

    /// Return a valid access token, refreshing it if necessary.
    pub async fn access_token(&self) -> anyhow::Result<String> {
        let path = expand_path(&self.token_file);

        // Fast path: use cached token if still fresh.
        {
            let guard = self.cached.lock().await;
            if let Some(token) = guard.as_ref()
                && !needs_refresh(token)
            {
                return Ok(token.access_token.clone());
            }
        }

        // Load from disk.
        let mut token = load_token(&path)
            .await?
            .with_context(|| format!("OAuth token file not found: {}", path.display()))?;

        if needs_refresh(&token) {
            if token.refresh_token.is_empty() {
                anyhow::bail!("OAuth token expired and no refresh token is available");
            }
            info!("refreshing OAuth token");
            token = self.refresh_token(&token.refresh_token).await?;
            save_token(&path, &token).await?;
        }

        let access = token.access_token.clone();
        let mut guard = self.cached.lock().await;
        *guard = Some(token);
        Ok(access)
    }

    async fn refresh_token(&self, refresh_token_str: &str) -> anyhow::Result<OAuthToken> {
        let client = reqwest::Client::new();

        let response = client
            .post(&self.config.token_endpoint)
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token_str),
            ])
            .send()
            .await
            .context("OAuth refresh request failed")?;

        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .context("failed to parse OAuth refresh response")?;

        if !status.is_success() {
            error!(status = %status, body = %body, "OAuth token refresh failed");
            if status.as_u16() == 401 || status.as_u16() == 403 {
                anyhow::bail!("OAuth refresh token was rejected; please log in again");
            }
            anyhow::bail!("OAuth token refresh failed (HTTP {}): {}", status, body);
        }

        let mut token: OAuthToken = serde_json::from_value(body)?;
        if token.expires_in > 0.0 && token.expires_at == 0.0 {
            token.expires_at = now_secs() + token.expires_in;
        }
        Ok(token)
    }
}

fn expand_path(path: &Path) -> PathBuf {
    if let Some(rest) = path.to_str().and_then(|s| s.strip_prefix("~/"))
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_path_buf()
}

async fn load_token(path: &Path) -> anyhow::Result<Option<OAuthToken>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read OAuth token file: {}", path.display()))?;
    let token: OAuthToken = serde_json::from_str(&text)
        .with_context(|| format!("invalid OAuth token file: {}", path.display()))?;
    Ok(Some(token))
}

async fn save_token(path: &Path, token: &OAuthToken) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("OAuth token file has no parent directory")?;
    tokio::fs::create_dir_all(parent).await?;

    let text = serde_json::to_string_pretty(token)?;
    let temp = parent.join(format!(".tmp-{}.json", uuid::Uuid::new_v4()));
    tokio::fs::write(&temp, text).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&temp).await?.permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(&temp, perms).await?;
    }

    tokio::fs::rename(&temp, path).await?;
    Ok(())
}

fn needs_refresh(token: &OAuthToken) -> bool {
    if token.expires_at <= 0.0 {
        return false;
    }
    let remaining = token.expires_at - now_secs();
    remaining < MIN_REFRESH_THRESHOLD_SECONDS
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
