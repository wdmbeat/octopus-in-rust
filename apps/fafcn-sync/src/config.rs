//! Persistent client configuration (server URL, gamedata dir).
//!
//! Stored at `%APPDATA%/fafcn-sync/config.toml` on Windows or
//! `$XDG_CONFIG_HOME/fafcn-sync/config.toml` / `~/.config/fafcn-sync/config.toml`
//! elsewhere, so players only pass `--server` / `--dir` once.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Remembered settings from previous runs.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Mirror base URL used last time.
    pub server: Option<String>,
    /// Local FAF gamedata directory used last time.
    pub gamedata_dir: Option<PathBuf>,
    /// FAF Client install folder (contains faf-client.exe), used for maps sync.
    #[serde(default)]
    pub faf_client_dir: Option<PathBuf>,
    /// GUI language ("zh" / "en"), remembered across launches.
    #[serde(default)]
    pub lang: Option<String>,
    /// Upload token for the uploader (remembered locally).
    #[serde(default)]
    pub upload_token: Option<String>,
    /// Uploader display name (remembered locally).
    #[serde(default)]
    pub uploader: Option<String>,
    /// Sync-tab checkbox: also sync the coop channel (large, opt-in).
    #[serde(default)]
    pub sync_coop: Option<bool>,
    /// Sync-tab checkbox: also sync the maps channel (large, opt-in).
    #[serde(default)]
    pub sync_maps: Option<bool>,
    /// Build tag of the client that last wrote this config. Used to detect
    /// the first run of a freshly downloaded/updated exe (see
    /// `with_embedded_defaults`).
    #[serde(default)]
    pub last_build_tag: Option<String>,
}

impl ClientConfig {
    /// Load the config file; missing or unparsable files yield defaults.
    pub fn load() -> Self {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist the config, creating the config directory if needed.
    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    /// Resolve the mirror address against the config the server embedded
    /// into this executable at download time (the mirror it came from).
    ///
    /// Priority: a remembered address wins over the embedded one, EXCEPT on
    /// the first run of a new build (`last_build_tag` differs from
    /// `build_tag`, including legacy configs that predate the field): a
    /// freshly downloaded or self-updated exe should talk to the mirror it
    /// came from — this is what repairs stale remembered addresses left by
    /// buggy old builds (e.g. a dead domain) without hardcoding any URL.
    /// The user's own edits are safe: after the first run the tag matches
    /// and the remembered address wins again. Dev builds carry no embedded
    /// config, so nothing changes there.
    ///
    /// Always stamps `last_build_tag` so the next save records it.
    pub fn with_embedded_defaults(mut self, build_tag: &str) -> Self {
        let embedded = read_embedded_config().and_then(|c| c.server);
        self.resolve_server(embedded, build_tag);
        self
    }

    /// The pure core of `with_embedded_defaults`, split out for testing.
    fn resolve_server(&mut self, embedded: Option<String>, build_tag: &str) {
        if let Some(embedded) = embedded {
            let first_run_of_build = self.last_build_tag.as_deref() != Some(build_tag);
            if first_run_of_build || self.server.is_none() {
                self.server = Some(embedded);
            }
        }
        self.last_build_tag = Some(build_tag.to_string());
    }
}

/// Read the config block the server appended to this executable, if any.
fn read_embedded_config() -> Option<fafcn_gamedata::EmbeddedConfig> {
    let exe = env::current_exe().ok()?;
    let bytes = fs::read(exe).ok()?;
    fafcn_gamedata::read_config(&bytes)
}

/// Platform config directory (`%APPDATA%/fafcn-sync` or `~/.config/fafcn-sync`).
fn config_dir() -> PathBuf {
    let base = env::var("APPDATA")
        .map(PathBuf::from)
        .or_else(|_| env::var("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|_| env::var("HOME").map(|h| Path::new(&h).join(".config")))
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("fafcn-sync")
}

/// Platform config file location.
fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Where GUI crash/exit reports are appended. GUI release builds have no
/// console, so a panic would otherwise make the window vanish silently.
pub fn crash_log_path() -> PathBuf {
    config_dir().join("fafcn-sync-log.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows paths with spaces and backslashes (e.g. the default FAF Client
    /// install location `C:\Program Files\FAF Client`) must survive the
    /// config.toml round-trip byte-for-byte: TOML basic strings treat `\` as
    /// an escape character, so a serialization bug here silently corrupts
    /// every remembered Windows path.
    #[test]
    fn windows_paths_with_spaces_survive_toml_round_trip() {
        let cfg = ClientConfig {
            server: Some("https://faforever.cn:60".to_string()),
            gamedata_dir: Some(PathBuf::from(r"C:\ProgramData\FAForever")),
            faf_client_dir: Some(PathBuf::from(r"C:\Program Files\FAF Client")),
            lang: Some("zh".to_string()),
            upload_token: Some("tok en".to_string()),
            uploader: Some("player one".to_string()),
            sync_coop: None,
            sync_maps: None,
            last_build_tag: Some("build-a".to_string()),
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: ClientConfig = toml::from_str(&text).unwrap();
        assert_eq!(
            back.faf_client_dir.as_deref(),
            Some(Path::new(r"C:\Program Files\FAF Client"))
        );
        assert_eq!(
            back.gamedata_dir.as_deref(),
            Some(Path::new(r"C:\ProgramData\FAForever"))
        );
        assert_eq!(back.upload_token.as_deref(), Some("tok en"));
    }

    const OLD: &str = "https://8v.pub:10041";
    const NEW: &str = "https://faforever.cn:60";

    /// Fresh download on a machine with no config: the embedded origin
    /// seeds the mirror address.
    #[test]
    fn fresh_install_adopts_embedded() {
        let mut cfg = ClientConfig::default();
        cfg.resolve_server(Some(NEW.to_string()), "build-b");
        assert_eq!(cfg.server.as_deref(), Some(NEW));
        assert_eq!(cfg.last_build_tag.as_deref(), Some("build-b"));
    }

    /// A stale remembered address left by a buggy old build (config predates
    /// `last_build_tag`) is repaired by the embedded origin of the new exe.
    #[test]
    fn legacy_config_is_repaired_on_first_run_of_new_build() {
        let mut cfg = ClientConfig {
            server: Some(OLD.to_string()),
            last_build_tag: None, // written before the field existed
            ..Default::default()
        };
        cfg.resolve_server(Some(NEW.to_string()), "build-b");
        assert_eq!(cfg.server.as_deref(), Some(NEW));
    }

    /// Same for self-update: the remembered address was for the OLD build;
    /// the new exe's first run adopts its embedded origin.
    #[test]
    fn self_update_adopts_embedded_of_new_build() {
        let mut cfg = ClientConfig {
            server: Some(OLD.to_string()),
            last_build_tag: Some("build-a".to_string()),
            ..Default::default()
        };
        cfg.resolve_server(Some(NEW.to_string()), "build-b");
        assert_eq!(cfg.server.as_deref(), Some(NEW));
    }

    /// A user's deliberate edit survives restarts of the SAME build.
    #[test]
    fn user_edit_wins_within_same_build() {
        let mut cfg = ClientConfig {
            server: Some(NEW.to_string()),
            last_build_tag: Some("build-b".to_string()),
            ..Default::default()
        };
        cfg.resolve_server(Some(OLD.to_string()), "build-b");
        assert_eq!(cfg.server.as_deref(), Some(NEW));
    }

    /// No embedded config (dev build): the remembered address is untouched
    /// and the tag is still stamped.
    #[test]
    fn no_embedded_keeps_remembered() {
        let mut cfg = ClientConfig {
            server: Some(NEW.to_string()),
            last_build_tag: Some("build-a".to_string()),
            ..Default::default()
        };
        cfg.resolve_server(None, "build-b");
        assert_eq!(cfg.server.as_deref(), Some(NEW));
        assert_eq!(cfg.last_build_tag.as_deref(), Some("build-b"));
    }
}
