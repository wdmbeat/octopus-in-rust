//! Server-side auto-updater for official FAF releases.
//!
//! Three upstream sources are mirrored automatically:
//!
//! - **gamedata patches** — official patch version from `mod_info.lua`,
//!   archive files from the legacy-updater CDN;
//! - **FAF client installer** — latest GitHub release of
//!   `FAForever/downlords-faf-client` (Windows exe asset only);
//! - **map generator jars** — every GitHub release of
//!   `FAForever/Neroxis-Map-Generator` in the newest few version series:
//!   a player joining a game on a map generated with an older generator
//!   needs that exact jar, so older releases within the keep range are
//!   backfilled, not just the latest one.
//!
//! Two triggers share one [`UpdaterHandle::update_once`]:
//!
//! - a periodic poller ([`spawn_poller`], every [`POLL_INTERVAL`]), and
//! - `POST /api/gamedata/upstream/refresh`, called by the sync client at the
//!   start of every sync.
//!
//! Both go through a single-flight mutex plus a [`DEBOUNCE`] window so
//! concurrent player syncs cannot stampede the upstream download. Failures
//! are logged and recorded in the status snapshot — the updater never panics
//! and never blocks the rest of the server.

use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context};
use chrono::Utc;
use fafcn_gamedata::{
    compare_version_strings, map_generator_jar_version, map_generator_series, newest_jar_series,
    sha256_file, FileEntry, FileSyncRule, UpdaterComponent, UpdaterInfo, UpdaterState,
    UploadCommitRequest, CHANNEL_FAF_CLIENT, CHANNEL_GAMEDATA, CHANNEL_MAP_GENERATOR,
    GAMEDATA_FILES, MAP_GENERATOR_JAR_PREFIX, MAP_GENERATOR_KEEP_SERIES,
};
use futures::StreamExt;
use serde::Deserialize;

use crate::handlers::gamedata::GamedataStore;

/// How often the poller checks upstream for a new patch.
const POLL_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Minimum time between update attempts; concurrent sync clients within one
/// window share a single upstream check.
const DEBOUNCE: Duration = Duration::from_secs(30);

/// Where the current official patch version is published (`version = NNNN`).
const VERSION_URL: &str = "https://raw.githubusercontent.com/FAForever/fa/deploy/faf/mod_info.lua";

/// Base URL of the anonymous legacy-updater file downloads
/// (`{base}/{dir}.{version}.nx2`).
const BASE_URL: &str = "https://content.faforever.com/faf/updaterNew/updates_faf_files";

/// GitHub API endpoint for the latest FAF client release.
const CLIENT_RELEASE_API: &str =
    "https://api.github.com/repos/FAForever/downlords-faf-client/releases/latest";

/// GitHub API endpoint listing the Neroxis map generator releases (the same
/// endpoint the official client polls when opening the "generate map"
/// dialog). The LIST is mirrored — not just `/latest` — so older releases
/// within the keep range can be backfilled.
const GENERATOR_RELEASES_API: &str =
    "https://api.github.com/repos/FAForever/Neroxis-Map-Generator/releases?per_page=100";

/// Manifest uploader name for auto-committed patch sets.
const AUTO_UPLOADER: &str = "auto-updater";

/// An upstream release on GitHub, distilled to what we mirror.
#[derive(Debug, Clone)]
pub struct UpstreamRelease {
    /// Release version (tag without the leading `v`, e.g. `2026.7.1`).
    pub version: String,
    /// Asset file name of the file we mirror.
    pub file_name: String,
    /// Direct download URL of the asset.
    pub download_url: String,
}

/// The subset of the GitHub release JSON we read.
#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// One downloadable asset of a GitHub release.
#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Parse the GitHub latest-release JSON into a [`ClientRelease`].
///
/// The version comes from the tag (`v2026.7.1` → `2026.7.1`), NOT the file
/// name: `detect_version_from_filename` would misread `faf_windows-x64_…`
/// (the `x64` run merges into the digits). Asset pick: prefer the
/// `faf_windows*.exe` installer, fall back to the old `dfc_windows_*.exe`
/// naming; error when the release ships no Windows installer.
pub fn parse_client_release(json: &str) -> anyhow::Result<UpstreamRelease> {
    let release: GhRelease = serde_json::from_str(json).context("invalid GitHub release JSON")?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    let is_installer = |a: &&GhAsset| a.name.starts_with("faf_windows") && a.name.ends_with(".exe");
    let is_legacy = |a: &&GhAsset| a.name.starts_with("dfc_windows_") && a.name.ends_with(".exe");
    let asset = release
        .assets
        .iter()
        .find(is_installer)
        .or_else(|| release.assets.iter().find(is_legacy))
        .ok_or_else(|| {
            anyhow!(
                "release {} has no Windows installer asset",
                release.tag_name
            )
        })?;
    Ok(UpstreamRelease {
        version,
        file_name: asset.name.clone(),
        download_url: asset.browser_download_url.clone(),
    })
}

/// Parse the GitHub release-LIST JSON of `FAForever/Neroxis-Map-Generator`
/// into mirrorable releases, newest version first. Releases with a
/// non-numeric tag (prereleases like `1.23.0-rc1`) or without a
/// `NeroxisGen_*.jar` asset are skipped. The mirrored asset is the
/// `NeroxisGen_<version>.jar` the official client downloads
/// (`downloadUrlFormat` in its `application.yml`); the releases also ship
/// platform packages (rpm/dmg/exe) that we ignore.
pub fn parse_generator_releases(json: &str) -> anyhow::Result<Vec<UpstreamRelease>> {
    let releases: Vec<GhRelease> =
        serde_json::from_str(json).context("invalid GitHub releases JSON")?;
    let mut out = Vec::new();
    for release in releases {
        let version = release.tag_name.trim_start_matches('v').to_string();
        // Plain dotted-numeric tags only — a prerelease suffix would produce
        // a jar name the channel's version parsing rejects.
        if compare_version_strings(&version, "0").is_none() {
            continue;
        }
        let exact = format!("NeroxisGen_{version}.jar");
        let Some(asset) = release.assets.iter().find(|a| a.name == exact).or_else(|| {
            release
                .assets
                .iter()
                .find(|a| a.name.starts_with("NeroxisGen_") && a.name.ends_with(".jar"))
        }) else {
            continue;
        };
        out.push(UpstreamRelease {
            version,
            file_name: asset.name.clone(),
            download_url: asset.browser_download_url.clone(),
        });
    }
    // GitHub orders the list by creation date; we mirror by version.
    out.sort_by(|a, b| compare_version_strings(&b.version, &a.version).unwrap_or(Ordering::Equal));
    Ok(out)
}

/// Channel-relative path of the jar for a generator version.
fn generator_jar_path(version: &str) -> String {
    format!("{MAP_GENERATOR_JAR_PREFIX}{version}.jar")
}

/// Upstream HTTP fetches, injectable so unit tests need no network.
#[async_trait::async_trait]
pub trait UpstreamFetch: Send + Sync {
    /// Fetch the upstream `mod_info.lua` body.
    async fn fetch_mod_info(&self) -> anyhow::Result<String>;

    /// Fetch the latest FAF client release from GitHub.
    async fn fetch_latest_client_release(&self) -> anyhow::Result<UpstreamRelease>;

    /// Fetch the mirrorable Neroxis map generator releases from GitHub,
    /// newest version first.
    async fn fetch_generator_releases(&self) -> anyhow::Result<Vec<UpstreamRelease>>;

    /// Stream-download `url` to `dest`, verifying Content-Length when the
    /// server provides one. Returns the number of bytes written.
    async fn download_to(&self, url: &str, dest: &Path) -> anyhow::Result<u64>;
}

/// Production [`UpstreamFetch`] backed by reqwest.
struct HttpFetch {
    client: reqwest::Client,
}

#[async_trait::async_trait]
impl UpstreamFetch for HttpFetch {
    async fn fetch_mod_info(&self) -> anyhow::Result<String> {
        let body = self
            .client
            .get(VERSION_URL)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(body)
    }

    async fn fetch_latest_client_release(&self) -> anyhow::Result<UpstreamRelease> {
        // GitHub rejects API requests without a User-Agent header.
        let body = self
            .client
            .get(CLIENT_RELEASE_API)
            .header(reqwest::header::USER_AGENT, "fafcn-server")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        parse_client_release(&body)
    }

    async fn fetch_generator_releases(&self) -> anyhow::Result<Vec<UpstreamRelease>> {
        let body = self
            .client
            .get(GENERATOR_RELEASES_API)
            .header(reqwest::header::USER_AGENT, "fafcn-server")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        parse_generator_releases(&body)
    }

    async fn download_to(&self, url: &str, dest: &Path) -> anyhow::Result<u64> {
        let resp = self.client.get(url).send().await?.error_for_status()?;
        let expected = resp.content_length();
        let mut file = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("failed to create {}", dest.display()))?;
        let mut written = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
            written += chunk.len() as u64;
        }
        if let Some(expected) = expected {
            if written != expected {
                bail!("incomplete download of {url}: expected {expected} bytes, got {written}");
            }
        }
        Ok(written)
    }
}

/// Shared handle to the auto-updater; lives in [`crate::state::AppState`].
#[derive(Clone)]
pub struct UpdaterHandle {
    store: Arc<GamedataStore>,
    upstream: Arc<dyn UpstreamFetch>,
    status: Arc<RwLock<UpdaterInfo>>,
    /// Held for the whole duration of an update (single-flight).
    running: Arc<tokio::sync::Mutex<()>>,
    /// Last time an update was started (debounce window).
    last_attempt: Arc<Mutex<Option<Instant>>>,
}

impl UpdaterHandle {
    /// Create a handle that fetches from the real upstream endpoints.
    pub fn new(store: Arc<GamedataStore>) -> Self {
        Self::with_fetch(
            store,
            Arc::new(HttpFetch {
                client: reqwest::Client::new(),
            }),
        )
    }

    /// Create a handle with an injected upstream fetch (used by tests).
    pub fn with_fetch(store: Arc<GamedataStore>, upstream: Arc<dyn UpstreamFetch>) -> Self {
        Self {
            store,
            upstream,
            status: Arc::new(RwLock::new(UpdaterInfo {
                state: UpdaterState::Idle,
                latest_official_version: None,
                latest_client_version: None,
                latest_generator_version: None,
                last_check_at: None,
                last_error: None,
            })),
            running: Arc::new(tokio::sync::Mutex::new(())),
            last_attempt: Arc::new(Mutex::new(None)),
        }
    }

    /// Current updater status snapshot.
    pub fn snapshot(&self) -> UpdaterInfo {
        self.read_status()
    }

    /// Trigger an update check. Spawns the update in the background and
    /// returns the current status snapshot immediately; when an update is
    /// already running or the last attempt is inside the debounce window,
    /// this is a no-op snapshot.
    pub async fn trigger(&self, manual: bool) -> UpdaterInfo {
        let guard = self.running.clone().try_lock_owned().ok();
        let last = *self.last_attempt.lock().unwrap_or_else(|e| e.into_inner());
        if !should_start(guard.is_some(), last, Instant::now(), DEBOUNCE) {
            return self.snapshot();
        }
        *self.last_attempt.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        tracing::info!(manual, "starting gamedata upstream update check");
        let handle = self.clone();
        tokio::spawn(async move {
            // `guard` is Some exactly when `should_start` passed.
            let _guard = guard;
            handle.update_once().await;
        });
        self.snapshot()
    }

    /// One update pass: check both upstream sources (official gamedata
    /// patch, then the GitHub FAF client release) and mirror whatever is
    /// newer. The phases are independent: when one fails the other still
    /// runs, and the first error is recorded in `last_error`.
    async fn update_once(&self) {
        match self.run_update().await {
            Ok(()) => self.set_state(UpdaterState::Idle),
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "upstream auto-update failed");
                let mut status = self.write_status();
                status.state = UpdaterState::Idle;
                status.last_error = Some(format!("{err:#}"));
            }
        }
    }

    async fn run_update(&self) -> anyhow::Result<()> {
        self.set_state(UpdaterState::Checking);
        let mut first_error: Option<anyhow::Error> = None;
        for (phase, result) in [
            ("gamedata", self.update_gamedata().await),
            ("faf-client", self.update_faf_client().await),
            ("map-generator", self.update_map_generator().await),
        ] {
            if let Err(err) = result {
                tracing::warn!(phase, error = %format!("{err:#}"), "upstream phase failed");
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Check the official patch version, then download and commit when it
    /// is newer than the mirrored gamedata manifest.
    async fn update_gamedata(&self) -> anyhow::Result<()> {
        let body = self
            .upstream
            .fetch_mod_info()
            .await
            .context("failed to fetch upstream mod_info.lua")?;
        let version = parse_mod_version(&body)
            .ok_or_else(|| anyhow!("could not parse a version from upstream mod_info.lua"))?;
        {
            let mut status = self.write_status();
            status.latest_official_version = Some(version.clone());
            status.last_check_at = Some(Utc::now());
        }

        let store = self.store.clone();
        let current = tokio::task::spawn_blocking(move || store.read_manifest(CHANNEL_GAMEDATA))
            .await
            .context("task join error")?
            .map_err(|e| anyhow!("{e}"))?;
        let current_version = current.as_ref().map(|m| m.patch_version.clone());
        match current_version
            .as_deref()
            .and_then(|c| compare_version_strings(c, &version))
        {
            Some(Ordering::Equal) => {
                tracing::info!(version, "gamedata mirror already at latest official patch");
                return Ok(());
            }
            Some(Ordering::Greater) => {
                // Manual upload ahead of upstream; never downgrade.
                tracing::info!(
                    version,
                    current = current_version.as_deref().unwrap_or_default(),
                    "mirror is newer than upstream; skipping auto-update"
                );
                return Ok(());
            }
            _ => {}
        }

        self.set_state(UpdaterState::Downloading {
            component: UpdaterComponent::Gamedata,
            version: version.clone(),
        });
        let mut entries: Vec<FileEntry> = Vec::new();
        let mut tmps: Vec<PathBuf> = Vec::new();
        let result = self.download_all(&version, &mut entries, &mut tmps).await;
        if let Err(err) = result {
            for tmp in &tmps {
                let _ = std::fs::remove_file(tmp);
            }
            return Err(err);
        }

        for (entry, tmp) in entries.iter().zip(&tmps) {
            let store = self.store.clone();
            let entry = entry.clone();
            let tmp = tmp.clone();
            tokio::task::spawn_blocking(move || {
                store.store_file_from_path(CHANNEL_GAMEDATA, &entry.path, &entry.sha256, &tmp)
            })
            .await
            .context("task join error")?
            .map_err(|e| anyhow!("{e}"))?;
        }
        // Preserve manual-upload-only legacy extras (see
        // FileSyncRule::ManualPreserved): the gamedata channel never
        // prunes on-disk files, so the stored file and its recorded sha256
        // remain valid for commit re-validation.
        if let Some(manifest) = &current {
            for entry in &manifest.files {
                let preserved = GAMEDATA_FILES
                    .iter()
                    .find(|f| f.exact_name() == Some(entry.path.as_str()))
                    .is_some_and(|f| matches!(f.rule, FileSyncRule::ManualPreserved));
                if preserved {
                    entries.push(entry.clone());
                }
            }
        }
        let store = self.store.clone();
        let req = UploadCommitRequest {
            patch_version: version.clone(),
            uploader: AUTO_UPLOADER.to_string(),
            files: entries,
        };
        tokio::task::spawn_blocking(move || store.commit(CHANNEL_GAMEDATA, &req))
            .await
            .context("task join error")?
            .map_err(|e| anyhow!("{e}"))?;
        tracing::info!(version, "gamedata auto-update committed");
        Ok(())
    }

    /// Check the latest FAF client release on GitHub, then download and
    /// commit the Windows installer when it is newer than the mirrored
    /// faf-client manifest. The commit's prune-on-commit deletes the
    /// superseded installer automatically.
    async fn update_faf_client(&self) -> anyhow::Result<()> {
        let release = self
            .upstream
            .fetch_latest_client_release()
            .await
            .context("failed to fetch the latest FAF client release from GitHub")?;
        {
            let mut status = self.write_status();
            status.latest_client_version = Some(release.version.clone());
            status.last_check_at = Some(Utc::now());
        }

        let store = self.store.clone();
        let current = tokio::task::spawn_blocking(move || store.read_manifest(CHANNEL_FAF_CLIENT))
            .await
            .context("task join error")?
            .map_err(|e| anyhow!("{e}"))?
            .map(|m| m.patch_version);
        match current
            .as_deref()
            .and_then(|c| compare_version_strings(c, &release.version))
        {
            Some(Ordering::Equal) => {
                tracing::info!(
                    version = release.version,
                    "faf-client mirror already at latest release"
                );
                return Ok(());
            }
            Some(Ordering::Greater) => {
                // Manual upload ahead of upstream; never downgrade.
                tracing::info!(
                    version = release.version,
                    current = current.as_deref().unwrap_or_default(),
                    "mirror is newer than upstream; skipping auto-update"
                );
                return Ok(());
            }
            _ => {}
        }

        self.set_state(UpdaterState::Downloading {
            component: UpdaterComponent::FafClient,
            version: release.version.clone(),
        });
        let tmp = self
            .store
            .incoming_dir(CHANNEL_FAF_CLIENT)
            .join(format!("auto-{}.part", uuid::Uuid::new_v4()));
        let result = self.download_client_installer(&release, &tmp).await;
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    /// Download the installer to `tmp` (Content-Length check), hash it,
    /// store it under its release file name and commit the channel.
    async fn download_client_installer(
        &self,
        release: &UpstreamRelease,
        tmp: &Path,
    ) -> anyhow::Result<()> {
        tracing::info!(
            url = release.download_url,
            "downloading FAF client installer"
        );
        let written = self
            .upstream
            .download_to(&release.download_url, tmp)
            .await
            .with_context(|| format!("failed to download {}", release.download_url))?;
        let sha_tmp = tmp.to_path_buf();
        let sha256 = tokio::task::spawn_blocking(move || sha256_file(&sha_tmp))
            .await
            .context("task join error")??;
        let entry = FileEntry {
            path: release.file_name.clone(),
            size: written,
            sha256,
        };
        let store = self.store.clone();
        let store_entry = entry.clone();
        let store_tmp = tmp.to_path_buf();
        tokio::task::spawn_blocking(move || {
            store.store_file_from_path(
                CHANNEL_FAF_CLIENT,
                &store_entry.path,
                &store_entry.sha256,
                &store_tmp,
            )
        })
        .await
        .context("task join error")?
        .map_err(|e| anyhow!("{e}"))?;
        let store = self.store.clone();
        let req = UploadCommitRequest {
            patch_version: release.version.clone(),
            uploader: AUTO_UPLOADER.to_string(),
            files: vec![entry],
        };
        tokio::task::spawn_blocking(move || store.commit(CHANNEL_FAF_CLIENT, &req))
            .await
            .context("task join error")?
            .map_err(|e| anyhow!("{e}"))?;
        tracing::info!(
            version = release.version,
            "faf-client auto-update committed"
        );
        Ok(())
    }

    /// Check the Neroxis map generator releases on GitHub, then download and
    /// commit every missing jar in the newest [`MAP_GENERATOR_KEEP_SERIES`]
    /// version series — not just the latest release: a player joining a game
    /// on a map generated with an older generator needs that exact jar, so
    /// older releases within the keep range are backfilled onto the mirror.
    /// Prune-on-commit deletes jars whose series fell off the keep set.
    async fn update_map_generator(&self) -> anyhow::Result<()> {
        let releases = self
            .upstream
            .fetch_generator_releases()
            .await
            .context("failed to fetch the map generator releases from GitHub")?;
        let Some(latest) = releases.first() else {
            bail!("no mirrorable map generator release found");
        };
        {
            let mut status = self.write_status();
            status.latest_generator_version = Some(latest.version.clone());
            status.last_check_at = Some(Utc::now());
        }

        // Every release in the newest few version series is wanted.
        let keep_series = newest_jar_series(
            releases.iter().map(|r| r.version.as_str()),
            MAP_GENERATOR_KEEP_SERIES,
        );
        let wanted: Vec<&UpstreamRelease> = releases
            .iter()
            .filter(|r| keep_series.contains(map_generator_series(&r.version)))
            .collect();

        let store = self.store.clone();
        let existing =
            tokio::task::spawn_blocking(move || store.read_manifest(CHANNEL_MAP_GENERATOR))
                .await
                .context("task join error")?
                .map_err(|e| anyhow!("{e}"))?;
        if let Some(manifest) = &existing {
            if compare_version_strings(&manifest.patch_version, &latest.version)
                == Some(Ordering::Greater)
            {
                // Manual upload ahead of upstream; never downgrade.
                tracing::info!(
                    version = latest.version,
                    current = manifest.patch_version,
                    "mirror is newer than upstream; skipping auto-update"
                );
                return Ok(());
            }
        }
        let existing_paths: std::collections::HashSet<&str> = existing
            .as_ref()
            .map(|m| m.files.iter().map(|e| e.path.as_str()).collect())
            .unwrap_or_default();
        let missing: Vec<&UpstreamRelease> = wanted
            .into_iter()
            .filter(|r| !existing_paths.contains(generator_jar_path(&r.version).as_str()))
            .collect();
        if missing.is_empty()
            && existing
                .as_ref()
                .is_some_and(|m| m.patch_version == latest.version)
        {
            tracing::info!(
                version = latest.version,
                "map-generator mirror already at latest release"
            );
            return Ok(());
        }

        if !missing.is_empty() {
            self.set_state(UpdaterState::Downloading {
                component: UpdaterComponent::MapGenerator,
                version: latest.version.clone(),
            });
        }
        let mut new_entries: Vec<FileEntry> = Vec::new();
        for release in &missing {
            let tmp = self
                .store
                .incoming_dir(CHANNEL_MAP_GENERATOR)
                .join(format!("auto-{}.part", uuid::Uuid::new_v4()));
            match self.download_generator_jar(release, &tmp).await {
                Ok(entry) => new_entries.push(entry),
                Err(err) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(err);
                }
            }
        }

        // Commit every jar in the newest few series: the freshly downloaded
        // ones plus the previous manifest's entries (deduped by path).
        // Prune-on-commit removes whatever falls off the keep set (and
        // nothing else).
        let mut entries = new_entries;
        let new_paths: std::collections::HashSet<String> =
            entries.iter().map(|e| e.path.clone()).collect();
        if let Some(manifest) = existing {
            entries.extend(
                manifest
                    .files
                    .into_iter()
                    .filter(|e| !new_paths.contains(e.path.as_str())),
            );
        }
        let versions: Vec<String> = entries
            .iter()
            .filter_map(|e| map_generator_jar_version(&e.path))
            .collect();
        let keep = newest_jar_series(
            versions.iter().map(String::as_str),
            MAP_GENERATOR_KEEP_SERIES,
        );
        entries.retain(|e| {
            map_generator_jar_version(&e.path)
                .is_some_and(|v| keep.contains(map_generator_series(&v)))
        });
        entries.sort_by(|a, b| {
            // Newest first; both parse (the retain above dropped the rest).
            match (
                map_generator_jar_version(&b.path),
                map_generator_jar_version(&a.path),
            ) {
                (Some(vb), Some(va)) => {
                    compare_version_strings(&vb, &va).unwrap_or(Ordering::Equal)
                }
                _ => Ordering::Equal,
            }
        });

        // Drop entries whose file is no longer stored (defensive: commit
        // would reject the whole update otherwise).
        let store = self.store.clone();
        let check = entries.clone();
        let missing_files =
            tokio::task::spawn_blocking(move || store.check_needed(CHANNEL_MAP_GENERATOR, &check))
                .await
                .context("task join error")?
                .map_err(|e| anyhow!("{e}"))?;
        entries.retain(|e| !missing_files.contains(&e.path));

        let store = self.store.clone();
        let req = UploadCommitRequest {
            patch_version: latest.version.clone(),
            uploader: AUTO_UPLOADER.to_string(),
            files: entries,
        };
        tokio::task::spawn_blocking(move || store.commit(CHANNEL_MAP_GENERATOR, &req))
            .await
            .context("task join error")?
            .map_err(|e| anyhow!("{e}"))?;
        tracing::info!(
            version = latest.version,
            "map-generator auto-update committed"
        );
        Ok(())
    }

    /// Download the jar to `tmp` (Content-Length check), hash it and store
    /// it under the channel's `MapGenerator_<version>.jar` name. Returns the
    /// stored manifest entry.
    async fn download_generator_jar(
        &self,
        release: &UpstreamRelease,
        tmp: &Path,
    ) -> anyhow::Result<FileEntry> {
        tracing::info!(url = release.download_url, "downloading map generator jar");
        let written = self
            .upstream
            .download_to(&release.download_url, tmp)
            .await
            .with_context(|| format!("failed to download {}", release.download_url))?;
        let sha_tmp = tmp.to_path_buf();
        let sha256 = tokio::task::spawn_blocking(move || sha256_file(&sha_tmp))
            .await
            .context("task join error")??;
        let entry = FileEntry {
            path: generator_jar_path(&release.version),
            size: written,
            sha256,
        };
        let store = self.store.clone();
        let store_entry = entry.clone();
        let store_tmp = tmp.to_path_buf();
        tokio::task::spawn_blocking(move || {
            store.store_file_from_path(
                CHANNEL_MAP_GENERATOR,
                &store_entry.path,
                &store_entry.sha256,
                &store_tmp,
            )
        })
        .await
        .context("task join error")?
        .map_err(|e| anyhow!("{e}"))?;
        Ok(entry)
    }

    /// Download every [`FileSyncRule::PatchArchive`] file for `version`
    /// into the channel's `incoming/` dir, recording entries and temp paths
    /// for the caller to store/commit (or clean up on error).
    async fn download_all(
        &self,
        version: &str,
        entries: &mut Vec<FileEntry>,
        tmps: &mut Vec<PathBuf>,
    ) -> anyhow::Result<()> {
        for file in GAMEDATA_FILES {
            let name = match file.rule {
                FileSyncRule::PatchArchive => file
                    .exact_name()
                    .expect("PatchArchive rows are FileMatch::Exact (table test)"),
                // Manual-upload-only extras have no anonymous upstream; they
                // are carried forward by the caller's preserve step instead.
                // GitHubReleaseAsset/ManualOnly never occur in this table.
                FileSyncRule::ManualPreserved
                | FileSyncRule::GithubReleaseAsset
                | FileSyncRule::ManualOnly => continue,
            };
            let dir = name.strip_suffix(".nx2").unwrap_or(name);
            let url = format!("{BASE_URL}/{dir}.{version}.nx2");
            let tmp = self
                .store
                .incoming_dir(CHANNEL_GAMEDATA)
                .join(format!("auto-{}.part", uuid::Uuid::new_v4()));
            tracing::info!(url, "downloading upstream patch file");
            let written = self
                .upstream
                .download_to(&url, &tmp)
                .await
                .with_context(|| format!("failed to download {url}"))?;
            let sha_tmp = tmp.clone();
            let sha256 = tokio::task::spawn_blocking(move || sha256_file(&sha_tmp))
                .await
                .context("task join error")??;
            entries.push(FileEntry {
                path: name.to_string(),
                size: written,
                sha256,
            });
            tmps.push(tmp);
        }
        Ok(())
    }

    fn read_status(&self) -> UpdaterInfo {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn write_status(&self) -> std::sync::RwLockWriteGuard<'_, UpdaterInfo> {
        self.status.write().unwrap_or_else(|e| e.into_inner())
    }

    fn set_state(&self, state: UpdaterState) {
        self.write_status().state = state;
    }
}

/// Spawn the periodic upstream poller. The first check runs immediately.
pub fn spawn_poller(handle: UpdaterHandle) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            // `interval` ticks immediately the first time.
            interval.tick().await;
            handle.trigger(false).await;
        }
    });
}

/// Whether a trigger may start a new update: only when no update is running
/// and the last attempt is outside the debounce window.
fn should_start(
    running_guard_acquired: bool,
    last_attempt: Option<Instant>,
    now: Instant,
    debounce: Duration,
) -> bool {
    running_guard_acquired
        && last_attempt.is_none_or(|t| now.saturating_duration_since(t) >= debounce)
}

/// Extract the patch version from an upstream `mod_info.lua` body.
///
/// The implementation lives in `fafcn_gamedata::parse_mod_info_version` so
/// the sync client can parse the fa-coop `mod_info.lua` the same way.
pub use fafcn_gamedata::parse_mod_info_version as parse_mod_version;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[test]
    fn parse_mod_version_valid() {
        assert_eq!(
            parse_mod_version("name = \"FAF\"\nversion = 3838\n"),
            Some("3838".to_string())
        );
        assert_eq!(
            parse_mod_version("  version   =   3840  -- bumped\n"),
            Some("3840".to_string())
        );
        assert_eq!(parse_mod_version("version=3838"), Some("3838".to_string()));
    }

    #[test]
    fn parse_mod_version_garbage() {
        assert_eq!(parse_mod_version(""), None);
        assert_eq!(parse_mod_version("no version here"), None);
        assert_eq!(parse_mod_version("version = \"3838\""), None);
        assert_eq!(parse_mod_version("version_number = 3838"), None);
        assert_eq!(parse_mod_version("-- version = 3838"), None);
        assert_eq!(parse_mod_version("version = "), None);
    }

    #[test]
    fn debounce_and_single_flight_decision() {
        let now = Instant::now();
        // No guard (another update running) → never start.
        assert!(!should_start(false, None, now, DEBOUNCE));
        // Guard acquired, no previous attempt → start.
        assert!(should_start(true, None, now, DEBOUNCE));
        // Guard acquired, attempt inside the debounce window → no.
        assert!(!should_start(
            true,
            Some(now - Duration::from_secs(5)),
            now,
            DEBOUNCE
        ));
        // Guard acquired, debounce window expired → start.
        assert!(should_start(true, Some(now - DEBOUNCE), now, DEBOUNCE));
    }

    /// A realistic slice of the GitHub latest-release JSON.
    const RELEASE_JSON: &str = r#"{
        "tag_name": "v2026.7.1",
        "assets": [
            {
                "name": "faf_unix-universal_2026_7_1.tar.gz",
                "browser_download_url": "https://github.com/FAForever/downlords-faf-client/releases/download/v2026.7.1/faf_unix-universal_2026_7_1.tar.gz"
            },
            {
                "name": "faf_windows-x64_2026_7_1.zip",
                "browser_download_url": "https://github.com/FAForever/downlords-faf-client/releases/download/v2026.7.1/faf_windows-x64_2026_7_1.zip"
            },
            {
                "name": "faf_windows-x64_2026_7_1.exe",
                "browser_download_url": "https://github.com/FAForever/downlords-faf-client/releases/download/v2026.7.1/faf_windows-x64_2026_7_1.exe"
            }
        ]
    }"#;

    #[test]
    fn parse_client_release_picks_faf_windows_exe() {
        let release = parse_client_release(RELEASE_JSON).unwrap();
        // Version comes from the tag, not the file name.
        assert_eq!(release.version, "2026.7.1");
        assert_eq!(release.file_name, "faf_windows-x64_2026_7_1.exe");
        assert!(release
            .download_url
            .ends_with("/faf_windows-x64_2026_7_1.exe"));
    }

    #[test]
    fn parse_client_release_falls_back_to_legacy_dfc_naming() {
        let json = r#"{
            "tag_name": "v1.6.3",
            "assets": [
                {
                    "name": "dfc_unix-universal_1_6_3.tar.gz",
                    "browser_download_url": "https://example.test/dfc_unix-universal_1_6_3.tar.gz"
                },
                {
                    "name": "dfc_windows_1_6_3.exe",
                    "browser_download_url": "https://example.test/dfc_windows_1_6_3.exe"
                }
            ]
        }"#;
        let release = parse_client_release(json).unwrap();
        assert_eq!(release.version, "1.6.3");
        assert_eq!(release.file_name, "dfc_windows_1_6_3.exe");
    }

    #[test]
    fn parse_client_release_errors_without_windows_installer() {
        let json = r#"{
            "tag_name": "v2026.7.1",
            "assets": [
                {
                    "name": "faf_unix-universal_2026_7_1.tar.gz",
                    "browser_download_url": "https://example.test/faf_unix.tar.gz"
                }
            ]
        }"#;
        assert!(parse_client_release(json).is_err());
        assert!(parse_client_release("not json").is_err());
    }

    /// Fake upstream: serves a fixed `mod_info.lua` body and a map of
    /// download URL → file content; counts how often each fetch happened.
    /// Client version served by the default fake upstream.
    const FAKE_CLIENT_VERSION: &str = "2026.7.1";
    /// Installer file name served by the default fake upstream.
    const FAKE_CLIENT_FILE: &str = "faf_windows-x64_2026_7_1.exe";
    /// Installer URL served by the default fake upstream.
    const FAKE_CLIENT_URL: &str = "https://example.test/faf_windows-x64_2026_7_1.exe";
    /// Generator jar version served by the default fake upstream.
    const FAKE_GENERATOR_VERSION: &str = "1.22.1";
    /// Generator jar URL served by the default fake upstream.
    const FAKE_GENERATOR_URL: &str = "https://example.test/NeroxisGen_1.22.1.jar";

    struct FakeFetch {
        mod_info: String,
        /// `None` simulates a failed GitHub release fetch.
        client_release: Option<UpstreamRelease>,
        generator_releases: Option<Vec<UpstreamRelease>>,
        files: HashMap<String, Vec<u8>>,
        fetches: AtomicUsize,
        client_fetches: AtomicUsize,
        generator_fetches: AtomicUsize,
        downloads: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl UpstreamFetch for FakeFetch {
        async fn fetch_mod_info(&self) -> anyhow::Result<String> {
            self.fetches.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(self.mod_info.clone())
        }

        async fn fetch_latest_client_release(&self) -> anyhow::Result<UpstreamRelease> {
            self.client_fetches.fetch_add(1, AtomicOrdering::SeqCst);
            let release = self
                .client_release
                .as_ref()
                .ok_or_else(|| anyhow!("404: {CLIENT_RELEASE_API}"))?;
            Ok(release.clone())
        }

        async fn fetch_generator_releases(&self) -> anyhow::Result<Vec<UpstreamRelease>> {
            self.generator_fetches.fetch_add(1, AtomicOrdering::SeqCst);
            self.generator_releases
                .clone()
                .ok_or_else(|| anyhow!("404: {GENERATOR_RELEASES_API}"))
        }

        async fn download_to(&self, url: &str, dest: &Path) -> anyhow::Result<u64> {
            self.downloads.fetch_add(1, AtomicOrdering::SeqCst);
            let bytes = self.files.get(url).ok_or_else(|| anyhow!("404: {url}"))?;
            std::fs::write(dest, bytes)?;
            Ok(bytes.len() as u64)
        }
    }

    fn fake_fetch(version: &str) -> (Arc<FakeFetch>, Arc<dyn UpstreamFetch>) {
        fake_fetch_with_releases(
            version,
            Some(UpstreamRelease {
                version: FAKE_CLIENT_VERSION.to_string(),
                file_name: FAKE_CLIENT_FILE.to_string(),
                download_url: FAKE_CLIENT_URL.to_string(),
            }),
            fake_generator_releases(),
        )
    }

    fn fake_fetch_with_releases(
        version: &str,
        client_release: Option<UpstreamRelease>,
        generator_releases: Option<Vec<UpstreamRelease>>,
    ) -> (Arc<FakeFetch>, Arc<dyn UpstreamFetch>) {
        let mut files = HashMap::new();
        for file in GAMEDATA_FILES {
            if file.rule != FileSyncRule::PatchArchive {
                continue;
            }
            let name = file.exact_name().unwrap();
            let dir = name.strip_suffix(".nx2").unwrap_or(name);
            files.insert(
                format!("{BASE_URL}/{dir}.{version}.nx2"),
                format!("{name}-{version}").into_bytes(),
            );
        }
        for release in client_release
            .iter()
            .chain(generator_releases.iter().flatten())
        {
            files.insert(
                release.download_url.clone(),
                format!("asset-{}", release.version).into_bytes(),
            );
        }
        let fetch = Arc::new(FakeFetch {
            mod_info: format!("name = \"FAF\"\nversion = {version}\n"),
            client_release,
            generator_releases,
            files,
            fetches: AtomicUsize::new(0),
            client_fetches: AtomicUsize::new(0),
            generator_fetches: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
        });
        let shared: Arc<dyn UpstreamFetch> = fetch.clone();
        (fetch, shared)
    }

    /// The generator releases served by the default fake upstream.
    fn fake_generator_releases() -> Option<Vec<UpstreamRelease>> {
        Some(vec![UpstreamRelease {
            version: FAKE_GENERATOR_VERSION.to_string(),
            file_name: format!("NeroxisGen_{FAKE_GENERATOR_VERSION}.jar"),
            download_url: FAKE_GENERATOR_URL.to_string(),
        }])
    }

    fn temp_store() -> (PathBuf, Arc<GamedataStore>) {
        let root =
            std::env::temp_dir().join(format!("fafcn-updater-test-{}", uuid::Uuid::new_v4()));
        let store = GamedataStore::new(root.clone(), None).unwrap();
        (root, Arc::new(store))
    }

    /// Wait until the updater fetched upstream once and is `Idle` again
    /// (i.e. the background update ran to completion), or give up.
    async fn wait_finished(handle: &UpdaterHandle, fake: &FakeFetch) -> UpdaterInfo {
        for _ in 0..500 {
            let info = handle.snapshot();
            if fake.fetches.load(AtomicOrdering::SeqCst) > 0 && info.state == UpdaterState::Idle {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "update did not finish in time: {:?}",
            handle.snapshot().state
        );
    }

    /// Number of auto-fetched patch archives in the gamedata file table.
    fn patch_archive_count() -> usize {
        GAMEDATA_FILES
            .iter()
            .filter(|f| f.rule == FileSyncRule::PatchArchive)
            .count()
    }

    /// Pre-commit a channel with a single file at `version`.
    fn commit_one(store: &GamedataStore, channel: &str, name: &str, version: &str, bytes: &[u8]) {
        let entry = FileEntry {
            path: name.to_string(),
            size: bytes.len() as u64,
            sha256: fafcn_gamedata::sha256_bytes(bytes),
        };
        store
            .store_upload(channel, name, &entry.sha256, bytes)
            .unwrap();
        store
            .commit(
                channel,
                &UploadCommitRequest {
                    patch_version: version.to_string(),
                    uploader: "tester".to_string(),
                    files: vec![entry],
                },
            )
            .unwrap();
    }

    #[tokio::test]
    async fn update_downloads_and_commits_new_version() {
        let (root, store) = temp_store();
        let (fake, shared) = fake_fetch("3838");
        let handle = UpdaterHandle::with_fetch(store.clone(), shared);

        handle.trigger(true).await;
        let info = wait_finished(&handle, &fake).await;
        assert_eq!(info.state, UpdaterState::Idle);
        assert_eq!(info.latest_official_version.as_deref(), Some("3838"));
        assert_eq!(
            info.latest_client_version.as_deref(),
            Some(FAKE_CLIENT_VERSION)
        );
        assert!(
            info.last_error.is_none(),
            "unexpected error: {:?}",
            info.last_error
        );
        assert!(info.last_check_at.is_some());
        assert_eq!(fake.fetches.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(fake.client_fetches.load(AtomicOrdering::SeqCst), 1);
        // 3 gamedata archives + 1 client installer + 1 generator jar.
        assert_eq!(
            fake.downloads.load(AtomicOrdering::SeqCst),
            patch_archive_count() + 2
        );

        let manifest = store.read_manifest(CHANNEL_GAMEDATA).unwrap().unwrap();
        assert_eq!(manifest.patch_version, "3838");
        assert_eq!(manifest.uploader, AUTO_UPLOADER);
        assert_eq!(manifest.files.len(), patch_archive_count());
        for entry in &manifest.files {
            assert!(store
                .files_dir(CHANNEL_GAMEDATA)
                .join(&entry.path)
                .is_file());
        }

        // The faf-client channel got the installer, committed under the tag
        // version (no leading `v`).
        let client = store.read_manifest(CHANNEL_FAF_CLIENT).unwrap().unwrap();
        assert_eq!(client.patch_version, FAKE_CLIENT_VERSION);
        assert_eq!(client.uploader, AUTO_UPLOADER);
        assert_eq!(client.files.len(), 1);
        assert_eq!(client.files[0].path, FAKE_CLIENT_FILE);
        assert!(store
            .files_dir(CHANNEL_FAF_CLIENT)
            .join(FAKE_CLIENT_FILE)
            .is_file());

        // A second immediate trigger is debounced: no further upstream fetch.
        let _ = handle.trigger(true).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(fake.fetches.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(fake.downloads.load(AtomicOrdering::SeqCst), 5);
        fs_remove(root);
    }

    #[tokio::test]
    async fn update_preserves_static_gamedata_extras() {
        let (root, store) = temp_store();
        // Pre-commit an older patch that additionally carries the frozen
        // legacy extra `faforever.faf` (manual-upload-only; the auto-updater
        // cannot fetch it and must carry it forward).
        let extra_bytes = b"legacy-frozen-faf";
        let extra = FileEntry {
            path: "faforever.faf".to_string(),
            size: extra_bytes.len() as u64,
            sha256: fafcn_gamedata::sha256_bytes(extra_bytes),
        };
        store
            .store_upload(
                CHANNEL_GAMEDATA,
                "env.nx2",
                &fafcn_gamedata::sha256_bytes(b"old"),
                b"old",
            )
            .unwrap();
        store
            .store_upload(
                CHANNEL_GAMEDATA,
                "faforever.faf",
                &extra.sha256,
                extra_bytes,
            )
            .unwrap();
        store
            .commit(
                CHANNEL_GAMEDATA,
                &UploadCommitRequest {
                    patch_version: "3837".to_string(),
                    uploader: "tester".to_string(),
                    files: vec![
                        FileEntry {
                            path: "env.nx2".to_string(),
                            size: 3,
                            sha256: fafcn_gamedata::sha256_bytes(b"old"),
                        },
                        extra.clone(),
                    ],
                },
            )
            .unwrap();

        let (fake, shared) = fake_fetch("3838");
        let handle = UpdaterHandle::with_fetch(store.clone(), shared);
        handle.trigger(true).await;
        let info = wait_finished(&handle, &fake).await;
        assert!(
            info.last_error.is_none(),
            "unexpected error: {:?}",
            info.last_error
        );

        let manifest = store.read_manifest(CHANNEL_GAMEDATA).unwrap().unwrap();
        assert_eq!(manifest.patch_version, "3838");
        assert_eq!(manifest.files.len(), patch_archive_count() + 1);
        let preserved = manifest
            .files
            .iter()
            .find(|e| e.path == "faforever.faf")
            .expect("faforever.faf must survive the auto-update");
        assert_eq!(preserved.sha256, extra.sha256);
        assert!(store
            .files_dir(CHANNEL_GAMEDATA)
            .join("faforever.faf")
            .is_file());
        fs_remove(root);
    }

    #[tokio::test]
    async fn update_skips_when_mirror_is_current_or_newer() {
        let (root, store) = temp_store();
        // Pre-commit all channels at the same versions as upstream.
        commit_one(&store, CHANNEL_GAMEDATA, "env.nx2", "3838", b"patch-bytes");
        commit_one(
            &store,
            CHANNEL_FAF_CLIENT,
            FAKE_CLIENT_FILE,
            FAKE_CLIENT_VERSION,
            b"installer-bytes",
        );
        commit_one(
            &store,
            CHANNEL_MAP_GENERATOR,
            "MapGenerator_1.22.1.jar",
            FAKE_GENERATOR_VERSION,
            b"jar-bytes",
        );

        let (fake, shared) = fake_fetch("3838");
        let handle = UpdaterHandle::with_fetch(store.clone(), shared);
        handle.trigger(true).await;
        let info = wait_finished(&handle, &fake).await;
        assert!(info.last_error.is_none());
        assert_eq!(fake.fetches.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(fake.client_fetches.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            fake.downloads.load(AtomicOrdering::SeqCst),
            0,
            "already at latest version: no downloads"
        );
        fs_remove(root);
    }

    #[tokio::test]
    async fn client_update_commits_newer_release_and_prunes_old_installer() {
        let (root, store) = temp_store();
        // Mirror has the old client; gamedata and the generator are current.
        commit_one(&store, CHANNEL_GAMEDATA, "env.nx2", "3838", b"patch-bytes");
        commit_one(
            &store,
            CHANNEL_MAP_GENERATOR,
            "MapGenerator_1.22.1.jar",
            FAKE_GENERATOR_VERSION,
            b"jar-bytes",
        );
        commit_one(
            &store,
            CHANNEL_FAF_CLIENT,
            "dfc_windows_1_6_3.exe",
            "1.6.3",
            b"old-installer",
        );
        assert!(root
            .join("channels/faf-client/files/dfc_windows_1_6_3.exe")
            .is_file());

        let (fake, shared) = fake_fetch("3838");
        let handle = UpdaterHandle::with_fetch(store.clone(), shared);
        handle.trigger(true).await;
        let info = wait_finished(&handle, &fake).await;
        assert!(info.last_error.is_none(), "{:?}", info.last_error);
        // Only the installer was downloaded (gamedata already current).
        assert_eq!(fake.downloads.load(AtomicOrdering::SeqCst), 1);

        let client = store.read_manifest(CHANNEL_FAF_CLIENT).unwrap().unwrap();
        assert_eq!(client.patch_version, FAKE_CLIENT_VERSION);
        assert_eq!(client.files.len(), 1);
        assert_eq!(client.files[0].path, FAKE_CLIENT_FILE);
        assert!(
            !root
                .join("channels/faf-client/files/dfc_windows_1_6_3.exe")
                .exists(),
            "old installer pruned on commit"
        );
        assert!(root
            .join("channels/faf-client/files")
            .join(FAKE_CLIENT_FILE)
            .is_file());
        fs_remove(root);
    }

    #[tokio::test]
    async fn client_release_fetch_failure_leaves_gamedata_phase_working() {
        let (root, store) = temp_store();
        // GitHub client-release fetch 404s; other phases must still run.
        let (fake, shared) = fake_fetch_with_releases("3838", None, fake_generator_releases());
        let handle = UpdaterHandle::with_fetch(store.clone(), shared);

        handle.trigger(true).await;
        let info = wait_finished(&handle, &fake).await;
        assert_eq!(info.state, UpdaterState::Idle);
        assert!(info.last_error.is_some(), "expected recorded error");
        assert!(info.latest_client_version.is_none());
        // The gamedata phase ran to completion regardless.
        assert_eq!(info.latest_official_version.as_deref(), Some("3838"));
        // 3 gamedata archives + 1 generator jar.
        assert_eq!(
            fake.downloads.load(AtomicOrdering::SeqCst),
            patch_archive_count() + 1
        );
        assert_eq!(
            store
                .read_manifest(CHANNEL_GAMEDATA)
                .unwrap()
                .map(|m| m.patch_version)
                .as_deref(),
            Some("3838")
        );
        fs_remove(root);
    }

    #[tokio::test]
    async fn failed_download_returns_to_idle_with_error() {
        let (root, store) = temp_store();
        let (fake, _) = fake_fetch("3900");
        // Remove one file from the fake upstream → 404 → gamedata fails.
        let mut files = fake.files.clone();
        files.remove(&format!("{BASE_URL}/units.3900.nx2"));
        let broken = Arc::new(FakeFetch {
            mod_info: fake.mod_info.clone(),
            client_release: fake.client_release.clone(),
            generator_releases: fake.generator_releases.clone(),
            files,
            fetches: AtomicUsize::new(0),
            client_fetches: AtomicUsize::new(0),
            generator_fetches: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
        });
        let handle = UpdaterHandle::with_fetch(store.clone(), broken.clone());

        handle.trigger(true).await;
        let info = wait_finished(&handle, &broken).await;
        assert_eq!(info.state, UpdaterState::Idle);
        assert!(info.last_error.is_some(), "expected recorded error");
        assert_eq!(info.latest_official_version.as_deref(), Some("3900"));
        assert!(store.read_manifest(CHANNEL_GAMEDATA).unwrap().is_none());
        // The faf-client phase is independent and still mirrored the client.
        assert_eq!(
            store
                .read_manifest(CHANNEL_FAF_CLIENT)
                .unwrap()
                .map(|m| m.patch_version)
                .as_deref(),
            Some(FAKE_CLIENT_VERSION)
        );
        fs_remove(root);
    }

    #[test]
    fn parse_generator_releases_picks_neroxis_jars_newest_first() {
        let json = r#"[
            {"tag_name": "1.21.0", "assets": [
                {"name": "NeroxisGen_1.21.0.jar", "browser_download_url": "https://example.test/NeroxisGen_1.21.0.jar"}
            ]},
            {"tag_name": "v1.22.1", "assets": [
                {"name": "neroxis-generator-1.22.1.exe", "browser_download_url": "https://example.test/x.exe"},
                {"name": "NeroxisGen_1.22.1.jar", "browser_download_url": "https://example.test/NeroxisGen_1.22.1.jar"}
            ]},
            {"tag_name": "1.23.0-rc1", "assets": [
                {"name": "NeroxisGen_1.23.0-rc1.jar", "browser_download_url": "https://example.test/rc.jar"}
            ]},
            {"tag_name": "1.20.0", "assets": []}
        ]"#;
        let releases = parse_generator_releases(json).unwrap();
        let versions: Vec<&str> = releases.iter().map(|r| r.version.as_str()).collect();
        // Sorted by version (not list order); the prerelease tag and the
        // jar-less release are skipped.
        assert_eq!(versions, vec!["1.22.1", "1.21.0"]);
        assert_eq!(releases[0].file_name, "NeroxisGen_1.22.1.jar");
        assert!(releases[0].download_url.ends_with("NeroxisGen_1.22.1.jar"));
        assert!(parse_generator_releases("not json").is_err());
        assert!(parse_generator_releases("[]").unwrap().is_empty());
    }

    #[tokio::test]
    async fn generator_update_keeps_newest_series_and_prunes_older() {
        let (root, store) = temp_store();
        // Mirror already has jars across four series (two jars in 1.21.x);
        // gamedata/client are current.
        commit_one(&store, CHANNEL_GAMEDATA, "env.nx2", "3838", b"patch-bytes");
        commit_one(
            &store,
            CHANNEL_FAF_CLIENT,
            FAKE_CLIENT_FILE,
            FAKE_CLIENT_VERSION,
            b"installer-bytes",
        );
        let mut jars = Vec::new();
        for (name, version) in [
            ("MapGenerator_1.19.0.jar", "1.19.0"),
            ("MapGenerator_1.20.0.jar", "1.20.0"),
            ("MapGenerator_1.21.0.jar", "1.21.0"),
            ("MapGenerator_1.21.1.jar", "1.21.1"),
            ("MapGenerator_1.22.0.jar", "1.22.0"),
        ] {
            let entry = FileEntry {
                path: name.to_string(),
                size: name.len() as u64,
                sha256: fafcn_gamedata::sha256_bytes(name.as_bytes()),
            };
            store
                .store_upload(CHANNEL_MAP_GENERATOR, name, &entry.sha256, name.as_bytes())
                .unwrap();
            jars.push(entry);
            // Commit each version so the manifest ends at 1.22.0.
            store
                .commit(
                    CHANNEL_MAP_GENERATOR,
                    &UploadCommitRequest {
                        patch_version: version.to_string(),
                        uploader: "tester".to_string(),
                        files: jars.clone(),
                    },
                )
                .unwrap();
        }

        let (fake, shared) = fake_fetch("3838");
        let handle = UpdaterHandle::with_fetch(store.clone(), shared);
        handle.trigger(true).await;
        let info = wait_finished(&handle, &fake).await;
        assert!(info.last_error.is_none(), "{:?}", info.last_error);
        assert_eq!(
            info.latest_generator_version.as_deref(),
            Some(FAKE_GENERATOR_VERSION)
        );
        // Only the generator jar was downloaded.
        assert_eq!(fake.downloads.load(AtomicOrdering::SeqCst), 1);

        let manifest = store.read_manifest(CHANNEL_MAP_GENERATOR).unwrap().unwrap();
        assert_eq!(manifest.patch_version, FAKE_GENERATOR_VERSION);
        assert_eq!(manifest.uploader, AUTO_UPLOADER);
        let paths: Vec<&str> = manifest.files.iter().map(|e| e.path.as_str()).collect();
        // Every jar in the newest 3 series (1.22.x, 1.21.x, 1.20.x) is kept,
        // including both 1.21.x jars; only the 1.19.x series falls off.
        assert_eq!(
            paths,
            vec![
                "MapGenerator_1.22.1.jar",
                "MapGenerator_1.22.0.jar",
                "MapGenerator_1.21.1.jar",
                "MapGenerator_1.21.0.jar",
                "MapGenerator_1.20.0.jar"
            ],
            "newest 3 series kept"
        );
        assert!(
            !root
                .join("channels/map-generator/files/MapGenerator_1.19.0.jar")
                .exists(),
            "oldest series pruned on commit"
        );
        fs_remove(root);
    }

    #[tokio::test]
    async fn generator_update_backfills_older_releases_in_keep_series() {
        let (root, store) = temp_store();
        // Mirror has only the latest jar; gamedata/client are current.
        commit_one(&store, CHANNEL_GAMEDATA, "env.nx2", "3838", b"patch-bytes");
        commit_one(
            &store,
            CHANNEL_FAF_CLIENT,
            FAKE_CLIENT_FILE,
            FAKE_CLIENT_VERSION,
            b"installer-bytes",
        );
        commit_one(
            &store,
            CHANNEL_MAP_GENERATOR,
            "MapGenerator_1.22.1.jar",
            FAKE_GENERATOR_VERSION,
            b"jar-bytes",
        );

        // Upstream additionally lists older releases within the newest 3
        // series — plus 1.19.0, which is beyond the keep range.
        let releases: Vec<UpstreamRelease> = ["1.22.1", "1.22.0", "1.21.0", "1.20.0", "1.19.0"]
            .iter()
            .map(|v| UpstreamRelease {
                version: v.to_string(),
                file_name: format!("NeroxisGen_{v}.jar"),
                download_url: format!("https://example.test/NeroxisGen_{v}.jar"),
            })
            .collect();
        let (fake, shared) = fake_fetch_with_releases(
            "3838",
            Some(UpstreamRelease {
                version: FAKE_CLIENT_VERSION.to_string(),
                file_name: FAKE_CLIENT_FILE.to_string(),
                download_url: FAKE_CLIENT_URL.to_string(),
            }),
            Some(releases),
        );
        let handle = UpdaterHandle::with_fetch(store.clone(), shared);
        handle.trigger(true).await;
        let info = wait_finished(&handle, &fake).await;
        assert!(info.last_error.is_none(), "{:?}", info.last_error);
        // The three missing jars in the keep range were backfilled; 1.19.0
        // was NOT downloaded.
        assert_eq!(fake.downloads.load(AtomicOrdering::SeqCst), 3);

        let manifest = store.read_manifest(CHANNEL_MAP_GENERATOR).unwrap().unwrap();
        assert_eq!(manifest.patch_version, FAKE_GENERATOR_VERSION);
        let paths: Vec<&str> = manifest.files.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "MapGenerator_1.22.1.jar",
                "MapGenerator_1.22.0.jar",
                "MapGenerator_1.21.0.jar",
                "MapGenerator_1.20.0.jar"
            ],
            "older releases backfilled, newest first"
        );
        for path in &paths {
            assert!(
                store.files_dir(CHANNEL_MAP_GENERATOR).join(path).is_file(),
                "{path} stored"
            );
        }
        assert!(
            !root
                .join("channels/map-generator/files/MapGenerator_1.19.0.jar")
                .exists(),
            "1.19.x is beyond the keep series"
        );
        fs_remove(root);
    }

    fn fs_remove(root: PathBuf) {
        let _ = std::fs::remove_dir_all(root);
    }
}
