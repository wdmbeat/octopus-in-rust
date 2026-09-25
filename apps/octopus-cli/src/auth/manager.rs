use crate::auth::{oauth, platforms};
use crate::config::OAuthRef;
use crate::exception::{OctopusError, Result};
use crate::llm::LLM;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct OAuthManager {
    /// In-memory cache of access tokens by key.
    access_tokens: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// In-process lock to prevent concurrent refresh attempts.
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    /// Tombstones for recently-rejected refresh tokens: (refresh_token, rejection_time).
    rejected_refresh_tokens: Arc<std::sync::Mutex<HashMap<String, (String, Instant)>>>,
}

/// A resolved credential for LLM authentication.
#[derive(Debug, Clone)]
pub enum ApiCredential {
    /// A short-lived OAuth access token.
    OAuthToken(String),
    /// A static API key.
    ApiKey(String),
}

impl ApiCredential {
    pub fn as_str(&self) -> &str {
        match self {
            Self::OAuthToken(s) | Self::ApiKey(s) => s.as_str(),
        }
    }
}

impl OAuthManager {
    pub fn new() -> Self {
        Self {
            access_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            rejected_refresh_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Resolve the effective API credential.
    ///
    /// If an OAuth reference is provided and a cached access token exists,
    /// returns the access token. Otherwise falls back to the static API key.
    pub fn resolve_api_key(
        &self,
        api_key: Option<String>,
        oauth_ref: Option<&OAuthRef>,
    ) -> Option<ApiCredential> {
        if let Some(ref_ref) = oauth_ref {
            let cache = self.access_tokens.lock().unwrap();
            if let Some(token) = cache.get(&ref_ref.key) {
                return Some(ApiCredential::OAuthToken(token.clone()));
            }
        }
        api_key.map(ApiCredential::ApiKey)
    }

    /// Ensure OAuth tokens are fresh.
    ///
    /// When `force` is true, always refresh regardless of expiry.
    /// Returns `true` if a refresh was performed or the token is valid,
    /// `false` if no OAuth is configured for this provider.
    pub async fn ensure_fresh(&self, llm: &LLM, force: bool) -> Result<bool> {
        let oauth_ref = match self.kimi_code_ref(llm) {
            Some(r) => r,
            None => return Ok(false),
        };

        // Load persisted token
        let token = match oauth::load_tokens(&oauth_ref.key) {
            Some(t) => t,
            None => return Ok(false),
        };

        // Check if refresh token was recently rejected
        if self.should_suppress_persisted_token(&oauth_ref.key, &token) {
            self.access_tokens.lock().unwrap().remove(&oauth_ref.key);

            if !self.can_retry_rejected_refresh_token(&oauth_ref.key, &token.refresh_token) {
                if force {
                    return Err(OctopusError::Other(
                        "Refresh token was recently rejected. Please log in again.".to_string(),
                    ));
                }
                return Ok(false);
            }
        } else {
            // Cache the valid access token
            self.cache_access_token(&oauth_ref.key, &token);
        }

        // Decide whether to refresh
        self.refresh_tokens(&oauth_ref, token, force).await
    }

    /// Log in to a platform.
    pub async fn login(&self, platform_id: &str) -> Result<()> {
        match platform_id {
            "kimi-code" => {
                let token = oauth::login_kimi_code().await?;
                self.cache_access_token(oauth::KIMI_CODE_OAUTH_KEY, &token);
                println!("Successfully logged in to Kimi Code.");
                Ok(())
            }
            _ => {
                if let Some(platform) = platforms::resolve_platform(platform_id) {
                    if platform.managed_oauth {
                        Err(OctopusError::Other(format!(
                            "OAuth login for '{}' is not yet supported.",
                            platform_id
                        )))
                    } else {
                        Err(OctopusError::Other(format!(
                            "Platform '{}' uses API key authentication. Set the API key in your config file.",
                            platform.name
                        )))
                    }
                } else {
                    Err(OctopusError::Other(format!(
                        "Unknown platform: '{}'",
                        platform_id
                    )))
                }
            }
        }
    }

    /// Log out from a platform.
    pub async fn logout(&self, platform_id: &str) -> Result<()> {
        let key = match platform_id {
            "kimi-code" => oauth::KIMI_CODE_OAUTH_KEY,
            _ => {
                return Err(OctopusError::Other(format!(
                    "Unknown platform: '{}'",
                    platform_id
                )));
            }
        };

        oauth::delete_tokens(key)?;
        self.access_tokens.lock().unwrap().remove(key);
        self.rejected_refresh_tokens.lock().unwrap().remove(key);
        println!("Successfully logged out from {}.", platform_id);
        Ok(())
    }

    // ============================================================================
    // Internal
    // ============================================================================

    fn kimi_code_ref(&self, llm: &LLM) -> Option<OAuthRef> {
        // Find the provider config that matches the current LLM's provider
        llm.provider_config.as_ref().and_then(|p| {
            if p.provider_type == crate::config::ProviderType::Kimi {
                p.oauth.clone()
            } else {
                None
            }
        })
    }

    fn cache_access_token(&self, key: &str, token: &oauth::OAuthToken) {
        self.access_tokens
            .lock()
            .unwrap()
            .insert(key.to_string(), token.access_token.clone());
    }

    fn should_suppress_persisted_token(&self, key: &str, token: &oauth::OAuthToken) -> bool {
        let rejected = self.rejected_refresh_tokens.lock().unwrap();
        if let Some((rejected_token, _)) = rejected.get(key) {
            return rejected_token == &token.refresh_token;
        }
        false
    }

    fn can_retry_rejected_refresh_token(&self, key: &str, refresh_token: &str) -> bool {
        let rejected = self.rejected_refresh_tokens.lock().unwrap();
        if let Some((rejected_token, time)) = rejected.get(key)
            && rejected_token == refresh_token
        {
            let elapsed = time.elapsed().as_secs();
            return elapsed >= oauth::UNAUTHORIZED_REFRESH_RETRY_COOLDOWN_SECONDS;
        }
        true
    }

    fn mark_refresh_token_rejected(&self, key: &str, refresh_token: &str) {
        self.rejected_refresh_tokens
            .lock()
            .unwrap()
            .insert(key.to_string(), (refresh_token.to_string(), Instant::now()));
    }

    fn clear_rejected_refresh_token(&self, key: &str) {
        self.rejected_refresh_tokens.lock().unwrap().remove(key);
    }

    async fn refresh_tokens(
        &self,
        ref_ref: &OAuthRef,
        token: oauth::OAuthToken,
        force: bool,
    ) -> Result<bool> {
        let refresh_token_value = token.refresh_token.clone();
        if refresh_token_value.is_empty() {
            return Ok(false);
        }

        // In-process lock to prevent thundering herd within the same process
        let _guard = self.refresh_lock.lock().await;

        // Re-check: another task may have already refreshed
        let current = match oauth::load_tokens(&ref_ref.key) {
            Some(t) => t,
            None => return Ok(false),
        };

        // If the token on disk is different, someone else refreshed it
        if current.refresh_token != refresh_token_value {
            self.clear_rejected_refresh_token(&ref_ref.key);
            self.cache_access_token(&ref_ref.key, &current);
            return Ok(true);
        }

        // Check refresh threshold
        if !force {
            let now = oauth::now_secs();
            if current.expires_at > now {
                let threshold = oauth::refresh_threshold(current.expires_in);
                if current.expires_at - now >= threshold {
                    return Ok(false);
                }
            }
        }

        // Perform refresh
        match oauth::refresh_token(&refresh_token_value).await {
            Ok(refreshed) => {
                self.clear_rejected_refresh_token(&ref_ref.key);
                oauth::save_tokens(&ref_ref.key, &refreshed)?;
                self.cache_access_token(&ref_ref.key, &refreshed);
                Ok(true)
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("401") || msg.contains("403") {
                    self.mark_refresh_token_rejected(&ref_ref.key, &refresh_token_value);
                    self.access_tokens.lock().unwrap().remove(&ref_ref.key);
                    if force {
                        return Err(e);
                    }
                }
                Ok(false)
            }
        }
    }
}

impl Default for OAuthManager {
    fn default() -> Self {
        Self::new()
    }
}
