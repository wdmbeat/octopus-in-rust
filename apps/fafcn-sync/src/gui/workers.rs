//! Background sync/upload workers: messages and the SyncApp methods that
//! start them and drain their progress into the log.

use std::{path::PathBuf, sync::mpsc::channel, thread};

use eframe::egui;
use fafcn_gamedata::UpdaterComponent;

use crate::{
    config::ClientConfig,
    sync::{self, SyncProgress, SyncSummary},
    upload::{self, UploadProgress, UploadSummary},
};

use super::{
    app::{ActionState, SyncApp},
    strings::*,
};

/// Messages from a background worker to the UI.
pub(super) enum WorkerMsg {
    Sync(SyncProgress),
    Upload(UploadProgress),
    /// The sync ran without a FAF Client folder; maps were skipped.
    MapsSkipped,
    SyncDone(Result<SyncSummary, String>),
    UploadDone(Result<UploadSummary, String>),
}

impl SyncApp {
    pub(super) fn start_sync(&mut self) {
        let server = self.server.trim().trim_end_matches('/').to_string();
        let dir = self.faf_root();
        let faf_client = PathBuf::from(self.faf_client_dir.trim());
        let faf_client = sync::is_valid_faf_client_dir(&faf_client).then_some(faf_client);
        let options = sync::SyncOptions {
            coop: self.sync_coop,
            maps: self.sync_maps,
        };
        let (tx, rx) = channel();
        self.worker = Some(rx);
        self.progress = (0, 0);
        self.speed = 0.0;
        self.log.clear();
        self.sync_state = ActionState::Running;
        let cfg = self.persisted_config();

        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(async {
                    let mut forward = |event| {
                        let _ = tx.send(WorkerMsg::Sync(event));
                    };
                    let summary =
                        sync::sync_gamedata(&server, &dir, &options, &mut forward).await?;
                    // Maps live below the FAF Client folder, not FAForever.
                    match &faf_client {
                        Some(root) if options.maps => {
                            sync::sync_maps(&server, root, &mut forward).await?;
                        }
                        Some(_) => {
                            // Maps checkbox off: nothing to do.
                        }
                        None if options.maps => {
                            let _ = tx.send(WorkerMsg::MapsSkipped);
                        }
                        None => {}
                    }
                    Ok(summary)
                })
                .map_err(|e: anyhow::Error| format!("{e:#}"));
            let _ = tx.send(WorkerMsg::SyncDone(result));
            let _ = cfg.save();
        });
    }

    pub(super) fn start_upload_maps(&mut self) {
        let server = self.server.trim().trim_end_matches('/').to_string();
        let token = self.token.trim().to_string();
        let folder = PathBuf::from(self.maps_dir.trim());
        let uploader = self.uploader.trim().to_string();
        let (tx, rx) = channel();
        self.worker = Some(rx);
        self.progress = (0, 0);
        self.speed = 0.0;
        self.log.clear();
        self.upload_state = ActionState::Running;
        let cfg = self.persisted_config();

        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(upload::upload_maps(
                    &server,
                    &token,
                    &folder,
                    &uploader,
                    &mut |event| {
                        let _ = tx.send(WorkerMsg::Upload(event));
                    },
                ))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(WorkerMsg::UploadDone(result));
            let _ = cfg.save();
        });
    }

    pub(super) fn start_upload(&mut self) {
        let server = self.server.trim().trim_end_matches('/').to_string();
        let dir = self.faf_root();
        let token = self.token.trim().to_string();
        let patch_version = self.patch_version.trim().to_string();
        let uploader = self.uploader.trim().to_string();
        let (tx, rx) = channel();
        self.worker = Some(rx);
        self.progress = (0, 0);
        self.speed = 0.0;
        self.log.clear();
        self.upload_state = ActionState::Running;
        let cfg = self.persisted_config();

        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(upload::upload_gamedata(
                    &server,
                    &token,
                    &dir,
                    if patch_version.is_empty() {
                        None
                    } else {
                        Some(patch_version.as_str())
                    },
                    &uploader,
                    &mut |event| {
                        let _ = tx.send(WorkerMsg::Upload(event));
                    },
                ))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(WorkerMsg::UploadDone(result));
            let _ = cfg.save();
        });
    }

    pub(super) fn start_upload_client(&mut self) {
        let server = self.server.trim().trim_end_matches('/').to_string();
        let token = self.token.trim().to_string();
        let file = PathBuf::from(self.client_file.trim());
        let version = self.client_version.trim().to_string();
        let uploader = self.uploader.trim().to_string();
        let (tx, rx) = channel();
        self.worker = Some(rx);
        self.progress = (0, 0);
        self.speed = 0.0;
        self.log.clear();
        self.upload_state = ActionState::Running;
        let cfg = self.persisted_config();

        thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(upload::upload_faf_client(
                    &server,
                    &token,
                    &file,
                    &version,
                    &uploader,
                    &mut |event| {
                        let _ = tx.send(WorkerMsg::Upload(event));
                    },
                ))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(WorkerMsg::UploadDone(result));
            let _ = cfg.save();
        });
    }

    /// Current field values as they should be remembered on disk.
    pub(super) fn persisted_config(&self) -> ClientConfig {
        let mut cfg = ClientConfig::load();
        cfg.server = Some(self.server.trim().trim_end_matches('/').to_string());
        // Stamp the running build, or the next launch would look like the
        // first run of a new build again and re-adopt the embedded address.
        cfg.last_build_tag = Some(crate::BUILD_TAG.to_string());
        cfg.gamedata_dir = Some(self.faf_root());
        cfg.lang = Some(self.lang.code().to_string());
        if !self.token.trim().is_empty() {
            cfg.upload_token = Some(self.token.trim().to_string());
        }
        if !self.uploader.trim().is_empty() {
            cfg.uploader = Some(self.uploader.trim().to_string());
        }
        let faf_client = PathBuf::from(self.faf_client_dir.trim());
        if sync::is_valid_faf_client_dir(&faf_client) {
            cfg.faf_client_dir = Some(faf_client);
        }
        cfg.sync_coop = Some(self.sync_coop);
        cfg.sync_maps = Some(self.sync_maps);
        cfg
    }

    pub(super) fn drain_worker(&mut self, ctx: &egui::Context) {
        let mut finished = false;
        if let Some(rx) = &self.worker {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerMsg::Sync(SyncProgress::Upstream(event)) => {
                        let line = match event {
                            sync::UpstreamEvent::Checking => log_upstream_checking(self.lang),
                            sync::UpstreamEvent::ServerDownloading { component, version } => {
                                match component {
                                    UpdaterComponent::MapGenerator => {
                                        log_generator_downloading(self.lang, &version)
                                    }
                                    _ => log_upstream_downloading(self.lang, &version),
                                }
                            }
                            sync::UpstreamEvent::UpToDate => log_upstream_up_to_date(self.lang),
                            sync::UpstreamEvent::WaitTimedOut { component, version } => {
                                match component {
                                    Some(UpdaterComponent::MapGenerator) => {
                                        log_generator_timeout(self.lang, version.as_deref())
                                    }
                                    _ => log_upstream_timeout(self.lang, version.as_deref()),
                                }
                            }
                            sync::UpstreamEvent::Skipped { reason } => {
                                log_upstream_skipped(self.lang, &reason)
                            }
                        };
                        self.log.push(line);
                    }
                    WorkerMsg::Sync(SyncProgress::ChannelStarted { channel }) => {
                        self.log.push(log_channel_started(self.lang, &channel));
                    }
                    WorkerMsg::Sync(SyncProgress::ChannelEmpty { channel }) => {
                        self.log.push(log_channel_empty(self.lang, &channel));
                    }
                    WorkerMsg::Sync(SyncProgress::ManifestLoaded {
                        channel,
                        patch_version,
                        uploader,
                        file_count,
                        total_bytes,
                    }) => {
                        self.log.push(log_manifest(
                            self.lang,
                            &channel,
                            &patch_version,
                            file_count,
                            total_bytes as f64 / 1e6,
                            &uploader,
                        ));
                    }
                    WorkerMsg::Sync(SyncProgress::PlanReady {
                        downloads,
                        total_bytes,
                        ..
                    }) => {
                        self.progress = (0, total_bytes);
                        self.speed = 0.0;
                        self.log
                            .push(log_plan(self.lang, downloads, total_bytes as f64 / 1e6));
                    }
                    WorkerMsg::Sync(SyncProgress::Bytes(update)) => {
                        self.progress = (update.done_bytes, update.total_bytes);
                        self.speed = update.bytes_per_sec;
                    }
                    WorkerMsg::Sync(SyncProgress::FileInstalled {
                        path, index, count, ..
                    }) => {
                        self.log.push(log_file(self.lang, index, count, &path));
                    }
                    WorkerMsg::Sync(SyncProgress::FileFailed {
                        path,
                        index,
                        count,
                        error,
                    }) => {
                        self.log
                            .push(log_file_failed(self.lang, index, count, &path, &error));
                    }
                    WorkerMsg::Sync(SyncProgress::Mirrored { path, .. }) => {
                        self.log.push(log_mirrored(self.lang, &path));
                    }
                    WorkerMsg::Sync(SyncProgress::Pruned { path, .. }) => {
                        self.log.push(log_pruned(self.lang, &path));
                    }
                    WorkerMsg::MapsSkipped => {
                        self.log.push(log_maps_skipped(self.lang));
                    }
                    WorkerMsg::SyncDone(Ok(summary)) => {
                        self.log
                            .push(log_sync_done(self.lang, summary.downloaded_files));
                        self.sync_state = ActionState::Succeeded;
                        finished = true;
                    }
                    WorkerMsg::SyncDone(Err(err)) => {
                        self.log.push(log_failed(self.lang, &err));
                        self.sync_state = ActionState::Failed;
                        finished = true;
                    }
                    WorkerMsg::Upload(UploadProgress::ChannelStarted { channel }) => {
                        self.log.push(log_channel_started(self.lang, &channel));
                    }
                    WorkerMsg::Upload(UploadProgress::ChannelSkipped { channel, reason }) => {
                        self.log
                            .push(log_channel_skipped(self.lang, &channel, &reason));
                    }
                    WorkerMsg::Upload(UploadProgress::Scanned {
                        files, total_bytes, ..
                    }) => {
                        self.log
                            .push(log_scanned(self.lang, files, total_bytes as f64 / 1e6));
                    }
                    WorkerMsg::Upload(UploadProgress::Scanning {
                        done_files,
                        total_files,
                        done_bytes,
                        total_bytes,
                    }) => {
                        // First hash-phase event: announce it once, then the
                        // progress bar carries the live numbers.
                        if !self.scanning {
                            self.scanning = true;
                            self.log.push(tr(self.lang, Txt::Hashing).to_string());
                        }
                        self.progress = (done_bytes, total_bytes);
                        self.progress_files = (done_files, total_files);
                        self.speed = 0.0;
                    }
                    WorkerMsg::Upload(UploadProgress::Needed {
                        needed,
                        total_bytes,
                        ..
                    }) => {
                        // Hash phase over (if any); no bar when there is
                        // nothing to upload.
                        self.scanning = false;
                        self.progress = (0, if needed == 0 { 0 } else { total_bytes });
                        self.speed = 0.0;
                        self.log.push(log_needed(self.lang, needed));
                    }
                    WorkerMsg::Upload(UploadProgress::Bytes(update)) => {
                        self.progress = (update.done_bytes, update.total_bytes);
                        self.speed = update.bytes_per_sec;
                    }
                    WorkerMsg::Upload(UploadProgress::FileUploaded {
                        path, index, count, ..
                    }) => {
                        self.log
                            .push(log_uploaded_file(self.lang, index, count, &path));
                    }
                    WorkerMsg::Upload(UploadProgress::Committed { channel, files, .. }) => {
                        self.log.push(log_committed(self.lang, &channel, files));
                    }
                    WorkerMsg::UploadDone(Ok(summary)) => {
                        self.log
                            .push(log_upload_done(self.lang, &summary.published));
                        self.upload_state = ActionState::Succeeded;
                        self.scanning = false;
                        finished = true;
                    }
                    WorkerMsg::UploadDone(Err(err)) => {
                        self.log.push(log_failed(self.lang, &err));
                        self.upload_state = ActionState::Failed;
                        self.scanning = false;
                        finished = true;
                    }
                }
            }
        }
        if self.worker.is_some() {
            // Keep repainting while the worker is alive so progress shows up.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if finished {
            self.worker = None;
        }
    }
}
