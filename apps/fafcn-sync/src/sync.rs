//! Gamedata sync core, shared by the CLI and the GUI.
//!
//! [`sync_gamedata`] syncs every channel (gamedata, map-generator) below the
//! local `FAForever` root against the server manifests, downloading only what
//! is missing or changed, and reports progress through a callback so both
//! frontends can present it their own way.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use fafcn_gamedata::{
    channel_subdir, compare_version_strings, map_folder_version, map_generator_jar_version,
    map_generator_series, newest_jar_series, sha256_bytes, sha256_file, validate_relative_path,
    FileEntry, Manifest, StatusResponse, UpdaterComponent, UpdaterInfo, UpdaterState, CHANNEL_COOP,
    CHANNEL_GAMEDATA, CHANNEL_MAPS, CHANNEL_MAP_GENERATOR, MAP_GENERATOR_KEEP_SERIES,
    SYNC_CHANNELS,
};
use walkdir::WalkDir;

use crate::{
    api,
    config::ClientConfig,
    progress::{format_bytes, format_speed, ProgressReporter, TransferUpdate},
    SyncArgs,
};

/// Temp directory (inside the channel dir) for in-progress downloads.
const TMP_DIR_NAME: &str = ".fafcn-sync-tmp";

/// How often to poll the mirror while it downloads an official patch.
const UPSTREAM_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long to wait at most for the mirror's upstream download to finish.
const UPSTREAM_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

/// Sub-events of the upstream (official release) check phase at sync start.
pub enum UpstreamEvent {
    /// Asking the mirror to check for newer official releases.
    Checking,
    /// The mirror is downloading `version` of a component; we wait for it.
    ServerDownloading {
        /// Which component is being downloaded (gamedata patch or map
        /// generator — the mirror-only FAF client installer is never waited
        /// on).
        component: UpdaterComponent,
        /// Version being downloaded.
        version: String,
    },
    /// The mirror already has the latest official releases.
    UpToDate,
    /// The mirror did not finish in time; the sync proceeds with the version
    /// the mirror currently has. One event per component still pending;
    /// `component: None` means the upstream check itself never finished.
    WaitTimedOut {
        /// Component we waited for.
        component: Option<UpdaterComponent>,
        /// Version we waited for, when known.
        version: Option<String>,
    },
    /// The upstream check failed (old server, network, …); the sync
    /// proceeds regardless — the check is best-effort.
    Skipped {
        /// Why the check was skipped.
        reason: String,
    },
}

/// Progress events emitted while syncing.
pub enum SyncProgress {
    /// Upstream (official patch) check at the start of a sync.
    Upstream(UpstreamEvent),
    /// Started working on a channel.
    ChannelStarted {
        /// Channel id.
        channel: String,
    },
    /// The mirror has nothing published for this channel yet.
    ChannelEmpty {
        /// Channel id.
        channel: String,
    },
    /// A channel manifest was fetched successfully.
    ManifestLoaded {
        /// Channel id.
        channel: String,
        /// Channel version, as declared by the uploader.
        patch_version: String,
        /// Display name of the uploader.
        uploader: String,
        /// Number of tracked files.
        file_count: usize,
        /// Total size of all tracked files.
        total_bytes: u64,
    },
    /// The local diff is complete; these downloads are about to start.
    PlanReady {
        /// Number of files to download (0 means already up to date).
        downloads: usize,
        /// Total bytes to download.
        total_bytes: u64,
    },
    /// Byte-level progress within the current download plan.
    Bytes(TransferUpdate),
    /// A single file finished downloading and was installed.
    FileInstalled {
        /// Manifest-relative path.
        path: String,
        /// 1-based index within this run.
        index: usize,
        /// Total files in this run.
        count: usize,
    },
    /// A single file failed to download and was skipped; the sync continues
    /// with the remaining files and the failure is retried on the next run.
    FileFailed {
        /// Manifest-relative path.
        path: String,
        /// 1-based index within this run.
        index: usize,
        /// Total files in this run.
        count: usize,
        /// Why the download failed.
        error: String,
    },
    /// A file was mirrored into the replay data folder (`replaydata/gamedata`),
    /// which the FAF client uses when playing back replays.
    Mirrored {
        /// Manifest-relative path.
        path: String,
    },
    /// An outdated local file was pruned (map-generator keeps only the
    /// newest few version series).
    Pruned {
        /// File name that was removed.
        path: String,
    },
}

/// What a finished sync did.
pub struct SyncSummary {
    /// Files downloaded and installed (0 = already up to date).
    pub downloaded_files: usize,
    /// Bytes downloaded.
    pub downloaded_bytes: u64,
    /// Local gamedata files not tracked by the manifest (left untouched).
    pub extra_files: Vec<String>,
}

/// Optional content switches for a sync run.
///
/// Both default to `false` (opt-in): the `coop` channel (voice-over banks)
/// and the `maps` channel are large and not needed to play regular games.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncOptions {
    /// Sync the `coop` channel (co-op missions + voice-overs, big).
    pub coop: bool,
    /// Sync the `maps` channel into the FAF Client folder.
    pub maps: bool,
}

/// Run the CLI `sync` subcommand (prints progress to stdout).
pub async fn run(args: SyncArgs) -> Result<()> {
    let options = SyncOptions {
        coop: args.with_coop,
        maps: args.with_maps,
    };
    let mut cfg = ClientConfig::load().with_embedded_defaults(crate::BUILD_TAG);
    let server = api::resolve_server(args.server, &cfg)?;
    let root = resolve_faf_dir(args.dir, &cfg)?;
    println!("Mirror:    {server}");
    println!("FAForever: {}", root.display());

    let summary = sync_gamedata(&server, &root, &options, &mut print_progress).await?;

    for extra in &summary.extra_files {
        println!("Note: {extra} is not in the mirror manifest (left untouched)");
    }
    if summary.downloaded_files > 0 {
        println!(
            "Downloaded {} file(s), {:.1} MB.",
            summary.downloaded_files,
            summary.downloaded_bytes as f64 / 1e6
        );
    }

    // Maps live below the FAF Client folder, not FAForever.
    match (
        options.maps,
        resolve_faf_client_dir(args.faf_client_dir, &cfg),
    ) {
        (true, Some(client_root)) => {
            sync_maps(&server, &client_root, &mut print_progress).await?;
            cfg.faf_client_dir = Some(client_root);
        }
        (true, None) => {
            println!(
                "FAF Client folder not found — skipping maps sync \
                 (pass --faf-client-dir once to enable it)"
            );
        }
        (false, _) => {
            println!("maps sync disabled (pass --with-maps to enable)");
        }
    }

    cfg.server = Some(server);
    cfg.gamedata_dir = Some(root);
    cfg.save()?;
    println!("Sync complete. You can start the FAF client now.");
    Ok(())
}

/// Progress printer for the CLI `sync` subcommand.
fn print_progress(event: SyncProgress) {
    match event {
        SyncProgress::Upstream(event) => match event {
            UpstreamEvent::Checking => println!("checking for official updates…"),
            UpstreamEvent::ServerDownloading { component, version } => match component {
                UpdaterComponent::MapGenerator => {
                    println!("server is downloading map generator v{version}, waiting for it…")
                }
                _ => println!("server is downloading official patch v{version}, waiting for it…"),
            },
            UpstreamEvent::UpToDate => println!("server is up to date"),
            UpstreamEvent::WaitTimedOut { component, version } => {
                let what = match component {
                    Some(UpdaterComponent::MapGenerator) => "map generator",
                    _ => "upstream patch",
                };
                println!(
                    "timed out waiting for {what} {}; syncing what the mirror has",
                    version.as_deref().unwrap_or("(unknown)")
                );
            }
            UpstreamEvent::Skipped { reason } => {
                println!("upstream check skipped ({reason}); continuing")
            }
        },
        SyncProgress::ChannelStarted { channel } => println!("== {channel} =="),
        SyncProgress::ChannelEmpty { channel } => {
            println!("mirror has no {channel} yet — ask an uploader to publish it")
        }
        SyncProgress::ManifestLoaded {
            patch_version,
            uploader,
            file_count,
            total_bytes,
            ..
        } => {
            println!(
                "version {patch_version} ({file_count} files, {:.1} MB), uploaded by {uploader}",
                total_bytes as f64 / 1e6,
            );
        }
        SyncProgress::PlanReady {
            downloads,
            total_bytes,
            ..
        } => {
            if downloads == 0 {
                println!("up to date");
            } else {
                println!(
                    "downloading {downloads} file(s), {:.1} MB",
                    total_bytes as f64 / 1e6
                );
            }
        }
        SyncProgress::Bytes(update) => print_transfer(&update),
        SyncProgress::FileInstalled {
            path, index, count, ..
        } => {
            // Pad to overwrite the live progress line above.
            println!("\r[{index}/{count}] {path:<60}");
        }
        SyncProgress::FileFailed {
            path,
            index,
            count,
            error,
        } => {
            println!("\r[{index}/{count}] FAILED, skipped {path}: {error}");
        }
        SyncProgress::Mirrored { path, .. } => println!("mirrored to replaydata: {path}"),
        SyncProgress::Pruned { path, .. } => println!("pruned old file: {path}"),
    }
}

/// Ask the mirror to check for newer official releases and, when it is
/// downloading them, wait (bounded) until the manifests catch up. Both
/// components that change synced files are waited on — the gamedata patch
/// and the map generator jars; the mirror-only FAF client installer never
/// is. Best-effort: any error is logged as a progress event and the sync
/// continues with whatever the mirror currently has.
pub async fn prepare_upstream(server: &str, progress: &mut dyn FnMut(SyncProgress)) {
    progress(SyncProgress::Upstream(UpstreamEvent::Checking));
    let http = reqwest::Client::new();
    let info = match fetch_upstream_refresh(&http, server).await {
        Ok(info) => info,
        Err(err) => {
            progress(SyncProgress::Upstream(UpstreamEvent::Skipped {
                reason: format!("{err:#}"),
            }));
            return;
        }
    };
    // Versions worth waiting for, per component. During Checking they are
    // only known from the server's PREVIOUS check — the poll loop refines
    // them once the fresh check finishes.
    let (mut wanted_gamedata, mut wanted_generator): (Option<String>, Option<String>) =
        match info.state {
            UpdaterState::Idle => {
                progress(SyncProgress::Upstream(UpstreamEvent::UpToDate));
                return;
            }
            UpdaterState::Checking => (info.latest_official_version, info.latest_generator_version),
            UpdaterState::Downloading { component, version } => {
                // The phases run sequentially server-side (gamedata →
                // faf-client → map-generator), so seed the other component
                // from the last known check; the mirrored-version check
                // clears it right away when the mirror already has it.
                let mut gamedata = info.latest_official_version;
                let mut generator = info.latest_generator_version;
                match component {
                    UpdaterComponent::Gamedata => gamedata = Some(version),
                    UpdaterComponent::MapGenerator => generator = Some(version),
                    UpdaterComponent::FafClient => {}
                }
                (gamedata, generator)
            }
        };
    // Becomes true once a wanted version turned out to be already mirrored:
    // then a lingering `Checking` state is not worth waiting out (the common
    // "everything current" case stays snappy). When nothing is known wanted
    // and the check is still running, keep waiting — a download may follow.
    let mut satisfied = false;
    let mut announced_gamedata: Option<String> = None;
    let mut announced_generator: Option<String> = None;
    let deadline = Instant::now() + UPSTREAM_WAIT_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            let mut reported = false;
            for (component, wanted) in [
                (UpdaterComponent::Gamedata, &wanted_gamedata),
                (UpdaterComponent::MapGenerator, &wanted_generator),
            ] {
                if wanted.is_some() {
                    reported = true;
                    progress(SyncProgress::Upstream(UpstreamEvent::WaitTimedOut {
                        component: Some(component),
                        version: wanted.clone(),
                    }));
                }
            }
            if !reported {
                progress(SyncProgress::Upstream(UpstreamEvent::WaitTimedOut {
                    component: None,
                    version: None,
                }));
            }
            return;
        }
        let status = match fetch_status(&http, server).await {
            Ok(status) => status,
            Err(err) => {
                progress(SyncProgress::Upstream(UpstreamEvent::Skipped {
                    reason: format!("{err:#}"),
                }));
                return;
            }
        };
        let updater = status.updater.unwrap_or(UpdaterInfo {
            state: UpdaterState::Idle,
            latest_official_version: None,
            latest_client_version: None,
            latest_generator_version: None,
            last_check_at: None,
            last_error: None,
        });
        match &updater.state {
            UpdaterState::Downloading {
                component: UpdaterComponent::Gamedata,
                version,
            } => wanted_gamedata = Some(version.clone()),
            UpdaterState::Downloading {
                component: UpdaterComponent::MapGenerator,
                version,
            } => wanted_generator = Some(version.clone()),
            UpdaterState::Downloading { .. } => {}
            UpdaterState::Checking => {
                if updater.latest_official_version.is_some() {
                    wanted_gamedata = updater.latest_official_version.clone();
                }
                if updater.latest_generator_version.is_some() {
                    wanted_generator = updater.latest_generator_version.clone();
                }
            }
            UpdaterState::Idle => {}
        }
        let mirrored = |channel: &str| {
            status
                .channels
                .iter()
                .find(|c| c.name == channel)
                .and_then(|c| c.manifest.as_ref())
                .map(|m| m.patch_version.as_str())
        };
        if !still_pending(wanted_gamedata.as_deref(), mirrored(CHANNEL_GAMEDATA)) {
            satisfied |= wanted_gamedata.is_some();
            wanted_gamedata = None;
        }
        if !still_pending(wanted_generator.as_deref(), mirrored(CHANNEL_MAP_GENERATOR)) {
            satisfied |= wanted_generator.is_some();
            wanted_generator = None;
        }
        if wanted_gamedata.is_none() && wanted_generator.is_none() {
            let checking = matches!(updater.state, UpdaterState::Checking);
            if !checking || satisfied {
                progress(SyncProgress::Upstream(UpstreamEvent::UpToDate));
                return;
            }
        }
        if updater.state == UpdaterState::Idle {
            // Finished without the mirror reaching the wanted version: the
            // update failed server-side — proceed with what is there.
            match updater.last_error {
                Some(reason) => progress(SyncProgress::Upstream(UpstreamEvent::Skipped { reason })),
                None => progress(SyncProgress::Upstream(UpstreamEvent::UpToDate)),
            }
            return;
        }
        for (component, wanted, announced) in [
            (
                UpdaterComponent::Gamedata,
                &wanted_gamedata,
                &mut announced_gamedata,
            ),
            (
                UpdaterComponent::MapGenerator,
                &wanted_generator,
                &mut announced_generator,
            ),
        ] {
            if let Some(version) = wanted {
                if announced.as_ref() != Some(version) {
                    *announced = Some(version.clone());
                    progress(SyncProgress::Upstream(UpstreamEvent::ServerDownloading {
                        component,
                        version: version.clone(),
                    }));
                }
            }
        }
        tokio::time::sleep(UPSTREAM_POLL_INTERVAL).await;
    }
}

/// Whether a wanted upstream version is still pending given the version the
/// mirror currently serves: pending when something is wanted and the mirror
/// has nothing yet or is strictly older. Unparseable versions cannot be
/// ordered — treat them as satisfied rather than waiting forever.
fn still_pending(wanted: Option<&str>, mirrored: Option<&str>) -> bool {
    match (wanted, mirrored) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(w), Some(m)) => compare_version_strings(m, w) == Some(std::cmp::Ordering::Less),
    }
}

/// `POST /api/gamedata/upstream/refresh`: trigger (debounced) the mirror's
/// upstream check and return its updater snapshot.
pub(crate) async fn fetch_upstream_refresh(
    http: &reqwest::Client,
    server: &str,
) -> Result<UpdaterInfo> {
    let url = api::api_url(server, "upstream/refresh");
    let resp = api::ensure_success(http.post(&url).send().await?).await?;
    Ok(resp.json::<UpdaterInfo>().await?)
}

/// `GET /api/gamedata/status`.
pub(crate) async fn fetch_status(http: &reqwest::Client, server: &str) -> Result<StatusResponse> {
    let url = api::api_url(server, "status");
    let resp = api::ensure_success(http.get(&url).send().await?).await?;
    Ok(resp.json::<StatusResponse>().await?)
}

/// Sync every channel below `faf_root` against the mirror at `server`.
///
/// The `coop` channel is synced only when `options.coop` is set.
pub async fn sync_gamedata(
    server: &str,
    faf_root: &Path,
    options: &SyncOptions,
    progress: &mut dyn FnMut(SyncProgress),
) -> Result<SyncSummary> {
    prepare_upstream(server, progress).await;
    let http = reqwest::Client::new();
    let mut summary = SyncSummary {
        downloaded_files: 0,
        downloaded_bytes: 0,
        extra_files: Vec::new(),
    };

    for channel in SYNC_CHANNELS {
        if *channel == CHANNEL_COOP && !options.coop {
            continue;
        }
        progress(SyncProgress::ChannelStarted {
            channel: channel.to_string(),
        });
        let subdir = channel_subdir(channel).expect("known channel");
        let target_dir = faf_root.join(subdir);

        let Some(manifest) = fetch_manifest(&http, server, channel).await? else {
            progress(SyncProgress::ChannelEmpty {
                channel: channel.to_string(),
            });
            continue;
        };
        progress(SyncProgress::ManifestLoaded {
            channel: channel.to_string(),
            patch_version: manifest.patch_version.clone(),
            uploader: manifest.uploader.clone(),
            file_count: manifest.files.len(),
            total_bytes: manifest.total_size(),
        });

        fs::create_dir_all(&target_dir)
            .with_context(|| format!("failed to create {}", target_dir.display()))?;
        let (downloaded, bytes) =
            sync_channel(&http, server, channel, &target_dir, &manifest, progress).await?;
        summary.downloaded_files += downloaded;
        summary.downloaded_bytes += bytes;

        match *channel {
            CHANNEL_GAMEDATA => {
                summary.extra_files = find_extra_files(&target_dir, &manifest);
                // The FAF client keeps a separate copy of gamedata for replay
                // playback (`replaydata/gamedata`); keep it identical so
                // watching a replay never triggers an official download.
                mirror_to_replaydata(faf_root, &manifest, progress)?;
            }
            CHANNEL_MAP_GENERATOR => {
                prune_old_jars(&target_dir, &manifest, progress)?;
            }
            _ => {}
        }
    }
    Ok(summary)
}

/// Sync the `maps` channel into `<faf_client_root>/maps_and_mods/maps`,
/// deleting local map folders that are older versions of maps in the
/// manifest. Returns the number of files downloaded (0 = up to date or the
/// channel is not published yet).
pub async fn sync_maps(
    server: &str,
    faf_client_root: &Path,
    progress: &mut dyn FnMut(SyncProgress),
) -> Result<usize> {
    progress(SyncProgress::ChannelStarted {
        channel: CHANNEL_MAPS.to_string(),
    });
    let http = reqwest::Client::new();
    let Some(manifest) = fetch_manifest(&http, server, CHANNEL_MAPS).await? else {
        progress(SyncProgress::ChannelEmpty {
            channel: CHANNEL_MAPS.to_string(),
        });
        return Ok(0);
    };
    // Older versions left in the manifest by merged uploads must not be
    // synced: they would be re-downloaded on every run only to be pruned
    // again below.
    let manifest = newest_maps_only(&manifest);
    progress(SyncProgress::ManifestLoaded {
        channel: CHANNEL_MAPS.to_string(),
        patch_version: manifest.patch_version.clone(),
        uploader: manifest.uploader.clone(),
        file_count: manifest.files.len(),
        total_bytes: manifest.total_size(),
    });

    let target_dir = maps_dir(faf_client_root);
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("failed to create {}", target_dir.display()))?;
    let (downloaded, _bytes) = sync_channel(
        &http,
        server,
        CHANNEL_MAPS,
        &target_dir,
        &manifest,
        progress,
    )
    .await?;
    prune_stale_map_versions(&target_dir, &manifest, progress)?;
    Ok(downloaded)
}

/// Drop manifest entries that are an older version of a map also present in
/// a newer version (`name.v0001` when `name.v0002` exists). Entries that
/// don't follow the `.vNNNN` convention (e.g. top-level `.zip` files) are
/// always kept.
fn newest_maps_only(manifest: &Manifest) -> Manifest {
    let mut newest: HashMap<&str, u32> = HashMap::new();
    for entry in &manifest.files {
        let top = entry.path.split('/').next().unwrap_or(&entry.path);
        if let Some((base, version)) = map_folder_version(top) {
            newest
                .entry(base)
                .and_modify(|v| *v = (*v).max(version))
                .or_insert(version);
        }
    }
    let files = manifest
        .files
        .iter()
        .filter(|entry| {
            let top = entry.path.split('/').next().unwrap_or(&entry.path);
            match map_folder_version(top) {
                Some((base, version)) => newest.get(base) == Some(&version),
                None => true,
            }
        })
        .cloned()
        .collect();
    Manifest {
        files,
        ..manifest.clone()
    }
}

/// Delete local map folders that are older versions of maps present in the
/// manifest (`name.v0001` when the manifest has `name.v0002`). Folders whose
/// map is absent from the manifest (the player's own maps) are never touched.
fn prune_stale_map_versions(
    dir: &Path,
    manifest: &Manifest,
    progress: &mut dyn FnMut(SyncProgress),
) -> Result<()> {
    // Newest version per map base name in the manifest.
    let mut newest: HashMap<&str, u32> = HashMap::new();
    for entry in &manifest.files {
        let top = entry.path.split('/').next().unwrap_or(&entry.path);
        if let Some((base, version)) = map_folder_version(top) {
            newest
                .entry(base)
                .and_modify(|v| *v = (*v).max(version))
                .or_insert(version);
        }
    }
    if newest.is_empty() {
        return Ok(());
    }
    for item in fs::read_dir(dir)? {
        let item = item?;
        if !item.file_type()?.is_dir() {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        if let Some((base, version)) = map_folder_version(&name) {
            if newest.get(base).is_some_and(|&max| version < max) {
                fs::remove_dir_all(item.path())?;
                progress(SyncProgress::Pruned { path: name });
            }
        }
    }
    Ok(())
}

/// Diff one channel's target dir against its manifest and download what's
/// missing or changed. Returns (files downloaded, bytes downloaded).
async fn sync_channel(
    http: &reqwest::Client,
    server: &str,
    channel: &str,
    target_dir: &Path,
    manifest: &Manifest,
    progress: &mut dyn FnMut(SyncProgress),
) -> Result<(usize, u64)> {
    let mut downloads: Vec<&FileEntry> = Vec::new();
    for entry in &manifest.files {
        validate_relative_path(&entry.path)
            .with_context(|| format!("manifest contains unsafe path: {}", entry.path))?;
        if !local_file_matches(target_dir, entry)? {
            downloads.push(entry);
        }
    }

    let total_bytes = downloads.iter().map(|e| e.size).sum();
    progress(SyncProgress::PlanReady {
        downloads: downloads.len(),
        total_bytes,
    });

    if downloads.is_empty() {
        return Ok((0, 0));
    }
    let tmp_dir = target_dir.join(TMP_DIR_NAME);
    fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("failed to create {}", tmp_dir.display()))?;
    let count = downloads.len();
    let mut reporter = ProgressReporter::new(total_bytes, progress, SyncProgress::Bytes);
    let mut installed = 0usize;
    let mut installed_bytes = 0u64;
    for (i, entry) in downloads.iter().enumerate() {
        let before = reporter.done_bytes();
        match download_one(
            http,
            server,
            channel,
            target_dir,
            &tmp_dir,
            entry,
            &mut reporter,
        )
        .await
        {
            Ok(()) => {
                installed += 1;
                installed_bytes += entry.size;
                reporter.snapshot();
                reporter.emit(SyncProgress::FileInstalled {
                    path: entry.path.clone(),
                    index: i + 1,
                    count,
                });
            }
            Err(err) => {
                // Skip the failed file instead of aborting the whole sync;
                // credit its remaining bytes so the progress bar completes.
                let downloaded = reporter.done_bytes() - before;
                reporter.add(entry.size.saturating_sub(downloaded));
                reporter.snapshot();
                reporter.emit(SyncProgress::FileFailed {
                    path: entry.path.clone(),
                    index: i + 1,
                    count,
                    error: format!("{err:#}"),
                });
            }
        }
    }
    fs::remove_dir_all(&tmp_dir).ok();
    Ok((installed, installed_bytes))
}

/// True when the local file exists with matching size and hash.
fn local_file_matches(dir: &Path, entry: &FileEntry) -> Result<bool> {
    let path = dir.join(&entry.path);
    if !path.is_file() {
        return Ok(false);
    }
    if fs::metadata(&path)?.len() != entry.size {
        return Ok(false);
    }
    let local_hash =
        sha256_file(&path).with_context(|| format!("failed to hash {}", path.display()))?;
    Ok(local_hash == entry.sha256)
}

/// Keep `FAForever/replaydata/gamedata` identical to the just-synced
/// `FAForever/gamedata`: the FAF client reads patch files from the replaydata
/// copy when playing back replays, and downloads any mismatch from the
/// official servers — the exact download this tool exists to avoid.
///
/// Copies are local-only (tmp file + atomic rename). Files whose gamedata
/// copy does not match the manifest (e.g. its download failed) are skipped,
/// never deleting or overwriting a working replaydata copy.
fn mirror_to_replaydata(
    faf_root: &Path,
    manifest: &Manifest,
    progress: &mut dyn FnMut(SyncProgress),
) -> Result<()> {
    let src_dir = faf_root.join("gamedata");
    let dst_dir = faf_root.join("replaydata").join("gamedata");
    fs::create_dir_all(&dst_dir)
        .with_context(|| format!("failed to create {}", dst_dir.display()))?;
    for entry in &manifest.files {
        validate_relative_path(&entry.path)
            .with_context(|| format!("manifest contains unsafe path: {}", entry.path))?;
        if local_file_matches(&dst_dir, entry)? {
            continue;
        }
        if !local_file_matches(&src_dir, entry)? {
            continue; // gamedata copy is missing/bad; nothing safe to mirror
        }
        let src = src_dir.join(&entry.path);
        let dst = dst_dir.join(&entry.path);
        let tmp = dst_dir.join(format!(
            ".mirror-{}-{}.part",
            std::process::id(),
            entry.path.replace('/', "_")
        ));
        fs::copy(&src, &tmp)
            .with_context(|| format!("failed to mirror {} to replaydata", entry.path))?;
        fs::rename(&tmp, &dst)
            .with_context(|| format!("failed to install replaydata copy of {}", entry.path))?;
        progress(SyncProgress::Mirrored {
            path: entry.path.clone(),
        });
    }
    Ok(())
}

/// Print one live progress line (overwrites itself via carriage return).
fn print_transfer(update: &TransferUpdate) {
    use std::io::Write;
    print!(
        "\r{:>5.1}%  {} / {}  {}    ",
        update.percent(),
        format_bytes(update.done_bytes),
        format_bytes(update.total_bytes),
        format_speed(update.bytes_per_sec),
    );
    let _ = std::io::stdout().flush();
}

/// Attempts per file before giving up. The mirror is a small VPS whose proxy
/// occasionally drops connections under a long sync, so transient failures
/// (timeouts, resets, 5xx, truncated bodies) are retried with a short
/// backoff. 404s are never retried — the file is genuinely absent and only
/// a re-upload fixes that.
const MAX_DOWNLOAD_ATTEMPTS: u32 = 3;

/// Outcome of one download attempt.
enum FetchOutcome {
    /// Downloaded and hash-verified bytes.
    Ok(Vec<u8>),
    /// 404 — the file is not on the server; retrying is pointless.
    Missing,
    /// Transient failure (timeout, reset, 5xx, truncation, hash mismatch).
    Failed(anyhow::Error),
}

/// Download one file (with retries), verify its hash, then move it into
/// place (the old file is removed only after the replacement verifies).
/// Chunk sizes are fed into `reporter` so byte-level progress can be emitted;
/// a failed attempt's bytes are rolled back so retries don't count double.
async fn download_one(
    http: &reqwest::Client,
    server: &str,
    channel: &str,
    dir: &Path,
    tmp_dir: &Path,
    entry: &FileEntry,
    reporter: &mut ProgressReporter<'_, SyncProgress>,
) -> Result<()> {
    let url = api::api_url(
        server,
        &format!(
            "channels/{channel}/files/{}",
            api::encode_relative_path(&entry.path)
        ),
    );
    let mut attempt = 0;
    let bytes = loop {
        attempt += 1;
        let before = reporter.done_bytes();
        match fetch_file(http, &url, entry, reporter).await {
            FetchOutcome::Ok(bytes) => break bytes,
            outcome => {
                // Roll back the failed attempt's bytes so the progress bar
                // does not over-count the retry.
                reporter.sub(reporter.done_bytes() - before);
                match outcome {
                    FetchOutcome::Missing => {
                        return Err(anyhow!(
                            "server does not have {} (404) — the mirror needs a re-upload",
                            entry.path
                        ));
                    }
                    FetchOutcome::Failed(err) => {
                        if attempt >= MAX_DOWNLOAD_ATTEMPTS {
                            return Err(err
                                .context(format!("failed to download {}", entry.path))
                                .context(format!(
                                    "gave up after {MAX_DOWNLOAD_ATTEMPTS} attempts"
                                )));
                        }
                        tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                    }
                    FetchOutcome::Ok(_) => unreachable!(),
                }
            }
        }
    };

    let tmp_path = tmp_dir.join(&entry.path);
    if let Some(parent) = tmp_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp_path, &bytes)?;

    let dest = dir.join(&entry.path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // Windows refuses to rename over an existing file; remove it only after
    // the replacement is fully downloaded and verified.
    if dest.exists() {
        fs::remove_file(&dest)?;
    }
    fs::rename(&tmp_path, &dest)?;
    Ok(())
}

/// One download attempt: GET the file, stream the body, verify size and
/// sha256 against the manifest entry.
async fn fetch_file(
    http: &reqwest::Client,
    url: &str,
    entry: &FileEntry,
    reporter: &mut ProgressReporter<'_, SyncProgress>,
) -> FetchOutcome {
    let resp = match http.get(url).send().await {
        Ok(resp) => resp,
        Err(err) => return FetchOutcome::Failed(err.into()),
    };
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return FetchOutcome::Missing;
    }
    let mut resp = match api::ensure_success(resp).await {
        Ok(resp) => resp,
        Err(err) => return FetchOutcome::Failed(err),
    };
    let mut bytes = Vec::with_capacity(entry.size as usize);
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                reporter.add(chunk.len() as u64);
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(err) => return FetchOutcome::Failed(err.into()),
        }
    }
    let actual = sha256_bytes(&bytes);
    if actual != entry.sha256 {
        return FetchOutcome::Failed(anyhow!(
            "downloaded {} but its hash does not match the manifest (expected {}, got {})",
            entry.path,
            entry.sha256,
            actual
        ));
    }
    FetchOutcome::Ok(bytes)
}

/// Local files not tracked by the manifest. Never deletes anything.
fn find_extra_files(dir: &Path, manifest: &Manifest) -> Vec<String> {
    let tracked: HashSet<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    let mut extras = Vec::new();
    for item in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if !item.file_type().is_file() {
            continue;
        }
        let rel = relative_slash_path(dir, item.path());
        if rel.starts_with(TMP_DIR_NAME) {
            continue;
        }
        if !tracked.contains(rel.as_str()) {
            extras.push(rel);
        }
    }
    extras
}

/// Keep every local `MapGenerator_*.jar` whose version series is among the
/// newest [`MAP_GENERATOR_KEEP_SERIES`] series (considering both local files
/// and the manifest), deleting jars from older series. A jar tracked by the
/// manifest is never deleted even when a newer local-only jar pushes its
/// series out of the keep set — deleting it would just re-download it on the
/// next sync. Only jar files matching the generator pattern are ever touched.
fn prune_old_jars(
    dir: &Path,
    manifest: &Manifest,
    progress: &mut dyn FnMut(SyncProgress),
) -> Result<()> {
    let manifest_versions: HashSet<String> = manifest
        .files
        .iter()
        .filter_map(|f| map_generator_jar_version(&f.path))
        .collect();
    let mut local_jars: Vec<(String, String)> = Vec::new(); // (file_name, version)
    for item in fs::read_dir(dir)? {
        let name = item?.file_name().to_string_lossy().into_owned();
        if let Some(v) = map_generator_jar_version(&name) {
            local_jars.push((name, v));
        }
    }
    let keep_series = newest_jar_series(
        manifest_versions
            .iter()
            .map(String::as_str)
            .chain(local_jars.iter().map(|(_, v)| v.as_str())),
        MAP_GENERATOR_KEEP_SERIES,
    );
    for (name, version) in local_jars {
        let kept = manifest_versions.contains(&version)
            || keep_series.contains(map_generator_series(&version));
        if !kept {
            fs::remove_file(dir.join(&name))?;
            progress(SyncProgress::Pruned { path: name });
        }
    }
    Ok(())
}

/// Convert an absolute path below `dir` to a forward-slash relative path.
pub fn relative_slash_path(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Fetch a channel manifest; `Ok(None)` when the channel was never published.
async fn fetch_manifest(
    http: &reqwest::Client,
    server: &str,
    channel: &str,
) -> Result<Option<Manifest>> {
    let url = api::api_url(server, &format!("channels/{channel}/manifest.json"));
    let resp = http.get(&url).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let resp = api::ensure_success(resp).await?;
    Ok(Some(resp.json::<Manifest>().await?))
}

/// True when `path` looks like the FAF data root: it is named `FAForever`
/// and has a `gamedata` subfolder containing `.nx2` patch files.
pub fn is_valid_faf_dir(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let name_ok = path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("FAForever"));
    name_ok && contains_nx2(&path.join("gamedata"))
}

/// True when the directory contains at least one `.nx2` file (top level).
fn contains_nx2(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut rd| {
            rd.any(|e| {
                e.ok()
                    .is_some_and(|e| e.path().extension().is_some_and(|ext| ext == "nx2"))
            })
        })
        .unwrap_or(false)
}

/// File whose presence marks a FAF Client install root.
const FAF_CLIENT_EXE: &str = "faf-client.exe";

/// True when `path` looks like the FAF Client install root (it contains
/// `faf-client.exe`).
pub fn is_valid_faf_client_dir(path: &Path) -> bool {
    path.is_dir() && path.join(FAF_CLIENT_EXE).is_file()
}

/// The maps folder below a FAF Client root: `maps_and_mods/maps`.
pub fn maps_dir(faf_client_root: &Path) -> PathBuf {
    faf_client_root.join("maps_and_mods").join("maps")
}

/// Find the FAF Client install root automatically: a folder containing
/// `faf-client.exe`, scanning drive roots and their immediate subfolders
/// (e.g. `E:\FAF Client`) plus the default install location two levels down
/// (`C:\Program Files\FAF Client`). Candidates that also contain
/// `uninstall.exe` or an existing `maps_and_mods` folder rank first.
pub fn autodetect_faf_client_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for letter in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if !root.is_dir() {
            continue;
        }
        candidates.push(root.clone());
        if let Ok(rd) = fs::read_dir(&root) {
            candidates.extend(
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir()),
            );
        }
        // The install4j default is <drive>:\Program Files\FAF Client — one
        // level deeper than the scan above reaches.
        for pf in ["Program Files", "Program Files (x86)"] {
            if let Ok(rd) = fs::read_dir(root.join(pf)) {
                candidates.extend(
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.is_dir()),
                );
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(&home));
        for sub in [".local/share", "Games"] {
            if let Ok(rd) = fs::read_dir(Path::new(&home).join(sub)) {
                candidates.extend(
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.is_dir()),
                );
            }
        }
    }
    let confidence = |p: &Path| {
        let mut score = 0;
        if p.join("uninstall.exe").is_file() {
            score += 1;
        }
        if p.join("maps_and_mods").is_dir() {
            score += 1;
        }
        score
    };
    candidates
        .iter()
        .filter(|c| is_valid_faf_client_dir(c))
        .max_by_key(|c| confidence(c))
        .cloned()
}

/// Resolve the FAF Client directory: CLI arg > remembered config > auto-detect.
/// Returns `None` when nothing usable is found (maps sync is then skipped).
fn resolve_faf_client_dir(arg: Option<PathBuf>, cfg: &ClientConfig) -> Option<PathBuf> {
    let candidate = arg
        .or_else(|| cfg.faf_client_dir.clone())
        .or_else(autodetect_faf_client_dir)?;
    is_valid_faf_client_dir(&candidate).then_some(candidate)
}

/// Directories worth checking when auto-detecting the FAForever folder.
pub fn autodetect_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        candidates.push(Path::new(&program_data).join("FAForever"));
    }
    // FAF also commonly ends up directly under a drive root.
    for letter in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if root.is_dir() {
            candidates.push(root.join("FAForever"));
            candidates.push(root.join("ProgramData").join("FAForever"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(Path::new(&home).join(".faforever"));
        candidates.push(Path::new(&home).join(".local/share/FAForever"));
    }
    candidates
}

/// Find the local FAForever folder automatically: the first candidate that
/// fully validates, else the first that merely exists.
pub fn autodetect_faf_dir() -> Option<PathBuf> {
    let candidates = autodetect_candidates();
    candidates
        .iter()
        .find(|c| is_valid_faf_dir(c))
        .or_else(|| candidates.iter().find(|c| c.is_dir()))
        .cloned()
}

/// Normalize a configured/entered path: if it points at the `gamedata`
/// subfolder (config from older versions), use its `FAForever` parent.
pub fn normalize_faf_dir(path: PathBuf) -> PathBuf {
    if path.file_name().is_some_and(|n| n == "gamedata") {
        if let Some(parent) = path.parent() {
            if parent
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("FAForever"))
            {
                return parent.to_path_buf();
            }
        }
    }
    path
}

/// Resolve the FAForever directory: CLI arg > remembered config > auto-detect.
fn resolve_faf_dir(arg: Option<PathBuf>, cfg: &ClientConfig) -> Result<PathBuf> {
    if let Some(dir) = arg.or_else(|| cfg.gamedata_dir.clone()) {
        return Ok(normalize_faf_dir(dir));
    }
    if let Some(detected) = autodetect_faf_dir() {
        println!("Auto-detected FAForever directory: {}", detected.display());
        return Ok(detected);
    }
    Err(anyhow!(
        "could not find your FAForever directory; pass it once with --dir <path> (e.g. --dir \"C:\\ProgramData\\FAForever\")"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_for(path: &str, bytes: &[u8]) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            size: bytes.len() as u64,
            sha256: sha256_bytes(bytes),
        }
    }

    fn manifest_with(files: Vec<FileEntry>) -> Manifest {
        Manifest {
            patch_version: "3838".to_string(),
            uploader: "tester".to_string(),
            generated_at: chrono::Utc::now(),
            files,
        }
    }

    fn temp_faf_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fafcn-sync-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("gamedata")).unwrap();
        root
    }

    #[test]
    fn upstream_wait_pending_logic() {
        // Wanted but nothing mirrored yet, or mirror strictly older → wait.
        assert!(still_pending(Some("3838"), None));
        assert!(still_pending(Some("3838"), Some("3837")));
        assert!(still_pending(Some("1.22.1"), Some("1.22.0")));
        // Mirrored at or beyond the wanted version → satisfied.
        assert!(!still_pending(Some("3838"), Some("3838")));
        assert!(!still_pending(Some("3838"), Some("3839")));
        // Nothing wanted → never pending.
        assert!(!still_pending(None, None));
        assert!(!still_pending(None, Some("3837")));
        // Unparseable versions cannot be ordered → don't wait forever.
        assert!(!still_pending(Some("abc"), Some("xyz")));
    }

    #[test]
    fn mirror_copies_matching_files_to_replaydata() {
        let root = temp_faf_root();
        let bytes = b"patch-bytes";
        let entry = entry_for("env.nx2", bytes);
        fs::write(root.join("gamedata/env.nx2"), bytes).unwrap();
        let manifest = manifest_with(vec![entry]);

        let mut events = Vec::new();
        mirror_to_replaydata(&root, &manifest, &mut |e| events.push(e)).unwrap();

        let replay_copy = root.join("replaydata/gamedata/env.nx2");
        assert_eq!(fs::read(&replay_copy).unwrap(), bytes);
        assert_eq!(events.len(), 1, "one Mirrored event");

        // Second run: replaydata already matches → no-op.
        let mut events = Vec::new();
        mirror_to_replaydata(&root, &manifest, &mut |e| events.push(e)).unwrap();
        assert!(events.is_empty());

        // Corrupted replay copy is re-mirrored from the good gamedata copy.
        fs::write(&replay_copy, b"corrupt").unwrap();
        let mut events = Vec::new();
        mirror_to_replaydata(&root, &manifest, &mut |e| events.push(e)).unwrap();
        assert_eq!(fs::read(&replay_copy).unwrap(), bytes);
        assert_eq!(events.len(), 1);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mirror_skips_when_gamedata_copy_is_bad() {
        let root = temp_faf_root();
        let entry = entry_for("env.nx2", b"expected");
        // gamedata copy does NOT match the manifest (e.g. download failed).
        fs::write(root.join("gamedata/env.nx2"), b"stale").unwrap();
        // Existing replay copy must be left untouched.
        let replay_dir = root.join("replaydata/gamedata");
        fs::create_dir_all(&replay_dir).unwrap();
        fs::write(replay_dir.join("env.nx2"), b"working-replay-copy").unwrap();
        let manifest = manifest_with(vec![entry]);

        let mut events = Vec::new();
        mirror_to_replaydata(&root, &manifest, &mut |e| events.push(e)).unwrap();

        assert_eq!(
            fs::read(replay_dir.join("env.nx2")).unwrap(),
            b"working-replay-copy"
        );
        assert!(events.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    /// Write empty generator jars into `root/map_generator/`.
    fn write_jars(root: &Path, names: &[&str]) -> PathBuf {
        let dir = root.join("map_generator");
        fs::create_dir_all(&dir).unwrap();
        for name in names {
            fs::write(dir.join(name), b"jar").unwrap();
        }
        dir
    }

    #[test]
    fn prune_keeps_every_jar_in_the_newest_series() {
        let root = temp_faf_root();
        // Three jars in the 1.22.x series, two in 1.21.x, one in 1.20.x, one
        // in the outdated 1.19.x series.
        let dir = write_jars(
            &root,
            &[
                "MapGenerator_1.22.2.jar",
                "MapGenerator_1.22.1.jar",
                "MapGenerator_1.22.0.jar",
                "MapGenerator_1.21.5.jar",
                "MapGenerator_1.21.0.jar",
                "MapGenerator_1.20.3.jar",
                "MapGenerator_1.19.9.jar",
            ],
        );
        let manifest = manifest_with(vec![
            entry_for("MapGenerator_1.22.2.jar", b"jar"),
            entry_for("MapGenerator_1.21.5.jar", b"jar"),
        ]);

        let mut pruned = Vec::new();
        prune_old_jars(&dir, &manifest, &mut |e| {
            if let SyncProgress::Pruned { path } = e {
                pruned.push(path);
            }
        })
        .unwrap();

        // Only the jar outside the newest 3 series (1.22/1.21/1.20) is pruned;
        // every jar within a kept series survives, however many there are.
        assert_eq!(pruned, vec!["MapGenerator_1.19.9.jar"]);
        for kept in [
            "MapGenerator_1.22.2.jar",
            "MapGenerator_1.22.1.jar",
            "MapGenerator_1.22.0.jar",
            "MapGenerator_1.21.5.jar",
            "MapGenerator_1.21.0.jar",
            "MapGenerator_1.20.3.jar",
        ] {
            assert!(dir.join(kept).is_file(), "{kept} must be kept");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn prune_never_deletes_manifest_jars() {
        let root = temp_faf_root();
        // The manifest is one series behind a newer local-only jar (e.g. the
        // official client fetched 1.23.0 while the mirror still serves 1.22).
        // Without the manifest protection the just-downloaded 1.20.0 jar would
        // be pruned and re-downloaded on every sync.
        let dir = write_jars(
            &root,
            &[
                "MapGenerator_1.23.0.jar",
                "MapGenerator_1.22.1.jar",
                "MapGenerator_1.21.0.jar",
                "MapGenerator_1.20.0.jar",
            ],
        );
        let manifest = manifest_with(vec![
            entry_for("MapGenerator_1.22.1.jar", b"jar"),
            entry_for("MapGenerator_1.21.0.jar", b"jar"),
            entry_for("MapGenerator_1.20.0.jar", b"jar"),
        ]);

        let mut pruned = Vec::new();
        prune_old_jars(&dir, &manifest, &mut |e| {
            if let SyncProgress::Pruned { path } = e {
                pruned.push(path);
            }
        })
        .unwrap();

        assert!(pruned.is_empty(), "nothing may be pruned: {pruned:?}");
        for kept in [
            "MapGenerator_1.23.0.jar",
            "MapGenerator_1.22.1.jar",
            "MapGenerator_1.21.0.jar",
            "MapGenerator_1.20.0.jar",
        ] {
            assert!(dir.join(kept).is_file(), "{kept} must be kept");
        }
        fs::remove_dir_all(&root).unwrap();
    }
}
