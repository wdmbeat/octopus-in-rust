//! The SyncApp model: window entry point, shared fields, tabs, and the
//! eframe::App implementation. Background work lives in `workers` and
//! `self_update`; text lives in `strings`.

use std::{
    path::PathBuf,
    sync::mpsc::{channel, Receiver},
    thread,
};

use anyhow::Result;
use eframe::egui;

use crate::{
    config::ClientConfig,
    progress::{format_bytes, format_speed},
    sync, version,
};

use super::{
    fonts::install_cjk_font,
    self_update::{SelfUpdate, UpdateMsg},
    strings::*,
    version_panel::PanelMsg,
    workers::WorkerMsg,
};

/// Launch the GUI. Blocks until the window closes.
pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 520.0])
            .with_min_inner_size([480.0, 440.0]),
        ..Default::default()
    };
    let title = format!("fafcn-sync · {}", crate::BUILD_TAG);
    eframe::run_native(
        &title,
        options,
        Box::new(|cc| {
            // Force dark theme: egui defaults to following the OS theme
            // (light on most Windows), which makes the status colors
            // unreadable and clashes with the fafcn-web UI.
            cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            // Log lines and status text must be copyable (bug reports!).
            cc.egui_ctx
                .all_styles_mut(|s| s.interaction.selectable_labels = true);
            install_cjk_font(cc);
            Ok(Box::new(SyncApp::new()))
        }),
    )
    .map_err(|e| anyhow::anyhow!("gui error: {e}"))
}

/// The app tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Sync,
    UploadPatch,
    UploadClient,
    UploadMaps,
    Settings,
}

/// What the background worker is doing (or did last).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActionState {
    Idle,
    Running,
    Succeeded,
    Failed,
}

pub(super) struct SyncApp {
    pub(super) lang: GuiLang,
    pub(super) tab: Tab,
    /// Tab shown on the previous frame (for tab-switch actions).
    pub(super) last_tab: Tab,
    // Shared fields (same values for both tabs).
    pub(super) server: String,
    pub(super) dir: String,
    // Upload-only fields.
    pub(super) token: String,
    pub(super) patch_version: String,
    pub(super) uploader: String,
    // Installer-upload fields (faf-client channel).
    pub(super) client_file: String,
    pub(super) client_version: String,
    // Maps-upload source folder (UploadMaps tab).
    pub(super) maps_dir: String,
    // FAF Client install folder for maps sync (Sync tab).
    pub(super) faf_client_dir: String,
    // Optional sync content (Sync tab checkboxes, both opt-in).
    pub(super) sync_coop: bool,
    pub(super) sync_maps: bool,
    // Patch version auto-detected from lua.nx2 (recomputed when dir changes).
    pub(super) detected_version: Option<String>,
    pub(super) detected_generator: Option<String>,
    pub(super) version_dir: String,
    // Server's current patch version (fetched while on the upload tab).
    pub(super) server_version: Option<String>,
    pub(super) status_rx: Option<Receiver<Option<String>>>,
    pub(super) last_status_check: String,
    // Self-update state (mirror's client build vs BUILD_TAG).
    pub(super) update: SelfUpdate,
    pub(super) update_rx: Option<Receiver<UpdateMsg>>,
    /// Mirror address the last update check ran against.
    pub(super) update_checked_server: String,
    /// The running update check was requested via the 检查更新 button — log
    /// the conclusion when it finishes (automatic checks stay silent).
    pub(super) update_check_manual: bool,
    // Version panel (sync tab): latest `/api/gamedata/status` snapshot and
    // the workers that fetch it.
    pub(super) panel_status: Option<fafcn_gamedata::StatusResponse>,
    /// Silent status fetch (startup / mirror address change).
    pub(super) panel_rx: Option<Receiver<PanelMsg>>,
    /// Mirror address the last silent status fetch ran against.
    pub(super) panel_checked_server: String,
    /// Manual 检查更新 upstream refresh (refresh + bounded status poll).
    pub(super) check_rx: Option<Receiver<PanelMsg>>,
    // Per-tab action state.
    pub(super) sync_state: ActionState,
    pub(super) upload_state: ActionState,
    pub(super) worker: Option<Receiver<WorkerMsg>>,
    /// (done_bytes, total_bytes) for the progress bar of the running action.
    pub(super) progress: (u64, u64),
    /// Smoothed transfer speed (bytes/sec) of the running action.
    pub(super) speed: f64,
    /// The running upload is in its local-hash phase (no bytes on the wire
    /// yet); the button label and progress bar say so instead of "上传".
    pub(super) scanning: bool,
    /// (done_files, total_files) during the hash phase.
    pub(super) progress_files: (usize, usize),
    pub(super) log: Vec<String>,
}

impl SyncApp {
    fn new() -> Self {
        let cfg = ClientConfig::load().with_embedded_defaults(crate::BUILD_TAG);
        let dir = cfg
            .gamedata_dir
            .clone()
            .map(sync::normalize_faf_dir)
            .or_else(sync::autodetect_faf_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            lang: GuiLang::from_config(cfg.lang.as_deref()),
            tab: Tab::Sync,
            last_tab: Tab::Sync,
            server: cfg.server.unwrap_or_default(),
            dir,
            token: cfg.upload_token.unwrap_or_default(),
            patch_version: String::new(),
            uploader: cfg.uploader.unwrap_or_default(),
            client_file: String::new(),
            client_version: String::new(),
            maps_dir: String::new(),
            faf_client_dir: cfg
                .faf_client_dir
                .clone()
                .or_else(sync::autodetect_faf_client_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            // Optional content is opt-in: coop voice-overs and maps are big.
            sync_coop: cfg.sync_coop.unwrap_or(false),
            sync_maps: cfg.sync_maps.unwrap_or(false),
            detected_version: None,
            detected_generator: None,
            version_dir: String::new(),
            server_version: None,
            status_rx: None,
            last_status_check: String::new(),
            update: SelfUpdate::Checking,
            update_rx: None,
            update_checked_server: String::new(),
            update_check_manual: false,
            panel_status: None,
            panel_rx: None,
            panel_checked_server: String::new(),
            check_rx: None,
            sync_state: ActionState::Idle,
            upload_state: ActionState::Idle,
            worker: None,
            progress: (0, 0),
            speed: 0.0,
            scanning: false,
            progress_files: (0, 0),
            log: Vec::new(),
        }
    }

    fn busy(&self) -> bool {
        self.worker.is_some()
            || matches!(
                self.update,
                SelfUpdate::Downloading { .. } | SelfUpdate::Restarting
            )
    }

    /// The FAForever root from the dir field (tolerates a gamedata subpath).
    pub(super) fn faf_root(&self) -> PathBuf {
        sync::normalize_faf_dir(PathBuf::from(self.dir.trim()))
    }

    /// Default the maps-upload source folder to the player's own maps folder
    /// (`<FAF Client>/maps_and_mods/maps`) when we know the FAF Client root.
    fn prefill_maps_dir(&mut self) {
        if !self.maps_dir.trim().is_empty() {
            return;
        }
        let root = PathBuf::from(self.faf_client_dir.trim());
        if sync::is_valid_faf_client_dir(&root) {
            let maps = sync::maps_dir(&root);
            if maps.is_dir() {
                self.maps_dir = maps.to_string_lossy().into_owned();
            }
        }
    }

    fn folder_status(&self) -> Option<(egui::Color32, &'static str)> {
        let dir = self.dir.trim();
        if dir.is_empty() {
            return None;
        }
        let path = self.faf_root();
        if sync::is_valid_faf_dir(&path) {
            Some((egui::Color32::LIGHT_GREEN, tr(self.lang, Txt::DirValid)))
        } else if path.is_dir() {
            Some((egui::Color32::YELLOW, tr(self.lang, Txt::DirSuspicious)))
        } else {
            Some((egui::Color32::LIGHT_RED, tr(self.lang, Txt::DirMissing)))
        }
    }

    /// Compact warning on tabs that need the FAForever folder when it is
    /// invalid; the field itself lives on the Settings tab.
    fn dir_warning(&self, ui: &mut egui::Ui) {
        if sync::is_valid_faf_dir(&self.faf_root()) {
            return;
        }
        ui.colored_label(
            egui::Color32::YELLOW,
            tr(self.lang, Txt::DirInvalidOpenSettings),
        );
    }

    fn shared_fields(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        match self.tab {
            // Mirror address and both folders live on the Settings tab.
            Tab::Settings => {
                ui.label(tr(self.lang, Txt::ServerLabel));
                ui.add_enabled_ui(!busy, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server)
                            .hint_text("https://your-mirror-address")
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(4.0);

                ui.label(tr(self.lang, Txt::DirLabel));
                ui.add_enabled_ui(!busy, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.dir)
                            .hint_text(r"C:\ProgramData\FAForever")
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button(tr(self.lang, Txt::Browse)).clicked() {
                            if let Some(path) = folder_picker(&self.dir).pick_folder() {
                                self.dir = path.to_string_lossy().into_owned();
                            }
                        }
                        if ui.button(tr(self.lang, Txt::Detect)).clicked() {
                            if let Some(path) = sync::autodetect_faf_dir() {
                                self.dir = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                });
                if let Some((color, text)) = self.folder_status() {
                    ui.colored_label(color, text);
                }
                ui.add_space(4.0);

                ui.label(tr(self.lang, Txt::FafClientDirLabel));
                ui.add_enabled_ui(!busy, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.faf_client_dir)
                            .hint_text(r"E:\FAF Client")
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button(tr(self.lang, Txt::Browse)).clicked() {
                            if let Some(path) = folder_picker(&self.faf_client_dir).pick_folder() {
                                self.faf_client_dir = path.to_string_lossy().into_owned();
                            }
                        }
                        if ui.button(tr(self.lang, Txt::Detect)).clicked() {
                            if let Some(path) = sync::autodetect_faf_client_dir() {
                                self.faf_client_dir = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                });
                let dir = PathBuf::from(self.faf_client_dir.trim());
                if sync::is_valid_faf_client_dir(&dir) {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        tr(self.lang, Txt::FafClientFound),
                    );
                } else if dir.as_os_str().is_empty() || !dir.is_dir() {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        tr(self.lang, Txt::FafClientDirMissing),
                    );
                } else {
                    ui.colored_label(egui::Color32::YELLOW, tr(self.lang, Txt::FafClientMissing));
                }
            }
            Tab::Sync => {
                self.dir_warning(ui);
            }
            Tab::UploadPatch | Tab::UploadClient | Tab::UploadMaps => {
                if self.tab == Tab::UploadPatch {
                    self.dir_warning(ui);
                }
                // Token + player name are needed by every upload tab.
                ui.label(tr(self.lang, Txt::TokenLabel));
                ui.add_enabled_ui(!busy, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.token)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(tr(self.lang, Txt::UploaderLabel));
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.uploader).desired_width(140.0));
                    });
                });
            }
        }
    }

    /// Patch version detected from `lua.nx2` (cached, refreshed on dir change).
    fn detected_patch_version(&mut self) -> Option<String> {
        if self.version_dir != self.dir {
            self.version_dir = self.dir.clone();
            let root = PathBuf::from(self.dir.trim());
            self.detected_version = version::detect_patch_version(&root.join("gamedata"));
            self.detected_generator = version::detect_generator_version(&root);
        }
        self.detected_version.clone()
    }

    /// The version that would be uploaded: detected wins, manual is fallback.
    fn effective_patch_version(&mut self) -> Option<String> {
        self.detected_patch_version().or_else(|| {
            let manual = self.patch_version.trim();
            if manual.is_empty() {
                None
            } else {
                Some(manual.to_string())
            }
        })
    }

    /// Kick off a one-shot fetch of the server's patch version when the
    /// patch upload tab is showing and the server address changed.
    fn maybe_refresh_server_version(&mut self) {
        if self.tab != Tab::UploadPatch {
            return;
        }
        let server = self.server.trim().trim_end_matches('/').to_string();
        if server.is_empty() || self.last_status_check == server {
            return;
        }
        self.last_status_check = server.clone();
        let (tx, rx) = channel();
        self.status_rx = Some(rx);
        thread::spawn(move || {
            let version = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(async move {
                    let url = crate::api::api_url(&server, "channels/gamedata/manifest.json");
                    let resp = reqwest::get(url).await.ok()?;
                    if !resp.status().is_success() {
                        return None;
                    }
                    resp.json::<fafcn_gamedata::Manifest>()
                        .await
                        .ok()?
                        .patch_version
                        .into()
                });
            let _ = tx.send(version);
        });
    }

    /// Some(true) when the server already has a strictly newer patch.
    fn server_is_newer(&mut self) -> bool {
        let local = self.effective_patch_version();
        match (self.server_version.as_deref(), local.as_deref()) {
            (Some(server), Some(local)) => {
                fafcn_gamedata::compare_version_strings(server, local)
                    == Some(std::cmp::Ordering::Greater)
            }
            _ => false,
        }
    }

    /// Fields still missing for the installer upload, as localized names.
    fn client_missing_fields(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.server.trim().is_empty() {
            missing.push(tr(self.lang, Txt::FieldServer));
        }
        if self.token.trim().is_empty() {
            missing.push(tr(self.lang, Txt::FieldToken));
        }
        if !PathBuf::from(self.client_file.trim()).is_file() {
            missing.push(tr(self.lang, Txt::FieldClientFile));
        }
        if self.client_version.trim().is_empty() {
            missing.push(tr(self.lang, Txt::VersionLabel));
        }
        if self.uploader.trim().is_empty() {
            missing.push(tr(self.lang, Txt::FieldUploader));
        }
        missing
    }

    /// Fields still missing for the current tab's action, as localized names.
    fn missing_fields(&mut self) -> Vec<&'static str> {
        if self.tab == Tab::UploadClient {
            return self.client_missing_fields();
        }
        // The settings tab has no action button.
        if self.tab == Tab::Settings {
            return Vec::new();
        }
        let mut missing = Vec::new();
        if self.server.trim().is_empty() {
            missing.push(tr(self.lang, Txt::FieldServer));
        }
        // The FAForever folder is only needed for sync and patch upload.
        if matches!(self.tab, Tab::Sync | Tab::UploadPatch) && !self.faf_root().is_dir() {
            missing.push(tr(self.lang, Txt::FieldDir));
        }
        if self.tab == Tab::UploadPatch {
            if self.token.trim().is_empty() {
                missing.push(tr(self.lang, Txt::FieldToken));
            }
            if self.effective_patch_version().is_none() {
                missing.push(tr(self.lang, Txt::FieldPatchVersion));
            }
            if self.uploader.trim().is_empty() {
                missing.push(tr(self.lang, Txt::FieldUploader));
            }
        }
        if self.tab == Tab::UploadMaps {
            if self.token.trim().is_empty() {
                missing.push(tr(self.lang, Txt::FieldToken));
            }
            if !PathBuf::from(self.maps_dir.trim()).is_dir() {
                missing.push(tr(self.lang, Txt::FieldMapsDir));
            }
            if self.uploader.trim().is_empty() {
                missing.push(tr(self.lang, Txt::FieldUploader));
            }
        }
        missing
    }

    fn action_button(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        let missing = self.missing_fields();
        let server_newer = self.tab == Tab::UploadPatch && self.server_is_newer();
        let can_run = !busy && missing.is_empty() && !server_newer;
        let running = match self.tab {
            Tab::Sync => self.sync_state == ActionState::Running,
            Tab::UploadPatch | Tab::UploadClient | Tab::UploadMaps => {
                self.upload_state == ActionState::Running
            }
            // Never rendered (the settings tab has no action button).
            Tab::Settings => false,
        };
        let label = match (self.tab, running) {
            (Tab::Sync, true) => tr(self.lang, Txt::Syncing),
            (Tab::Sync, false) => tr(self.lang, Txt::SyncNow),
            (Tab::UploadPatch, true) | (Tab::UploadClient, true) | (Tab::UploadMaps, true)
                if self.scanning =>
            {
                tr(self.lang, Txt::Hashing)
            }
            (Tab::UploadPatch, true) => tr(self.lang, Txt::Uploading),
            (Tab::UploadPatch, false) => tr(self.lang, Txt::UploadNow),
            (Tab::UploadClient, true) => tr(self.lang, Txt::Uploading),
            (Tab::UploadClient, false) => tr(self.lang, Txt::UploadClientNow),
            (Tab::UploadMaps, true) => tr(self.lang, Txt::Uploading),
            (Tab::UploadMaps, false) => tr(self.lang, Txt::UploadNow),
            (Tab::Settings, _) => tr(self.lang, Txt::TabSettings),
        };
        if ui
            .add_enabled(
                can_run,
                egui::Button::new(label).min_size(egui::vec2(f32::INFINITY, 40.0)),
            )
            .clicked()
        {
            match self.tab {
                Tab::Sync => self.start_sync(),
                Tab::UploadPatch => self.start_upload(),
                Tab::UploadClient => self.start_upload_client(),
                Tab::UploadMaps => self.start_upload_maps(),
                Tab::Settings => {}
            }
        }
        // Explain exactly why the button is disabled.
        if !busy && server_newer {
            let server_v = self.server_version.clone().unwrap_or_default();
            let local_v = self.effective_patch_version().unwrap_or_default();
            ui.colored_label(
                egui::Color32::LIGHT_GREEN,
                txt_server_newer(self.lang, &server_v, &local_v),
            );
        } else if !busy && !missing.is_empty() {
            ui.colored_label(
                egui::Color32::YELLOW,
                format!(
                    "{}{}",
                    tr(self.lang, Txt::MissingPrefix),
                    missing.join(", ")
                ),
            );
        } else if self.tab == Tab::Sync && self.sync_state == ActionState::Idle {
            ui.label(tr(self.lang, Txt::IdleHint));
        }
    }
}

/// A folder picker that opens at `current` when it is a valid directory.
fn folder_picker(current: &str) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new();
    let current = PathBuf::from(current.trim());
    if current.is_dir() {
        dialog.set_directory(current)
    } else {
        dialog
    }
}

impl eframe::App for SyncApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_worker(ui.ctx());
        self.drain_update(ui.ctx());
        self.drain_panel(ui.ctx());
        // Tab-switch actions: entering the maps upload tab prefills the
        // source folder from the located FAF Client install; leaving the
        // settings tab persists the edited fields.
        if self.tab != self.last_tab {
            let leaving = self.last_tab;
            self.last_tab = self.tab;
            if leaving == Tab::Settings {
                let _ = self.persisted_config().save();
            }
            if self.tab == Tab::UploadMaps {
                self.prefill_maps_dir();
            }
        }
        if let Some(rx) = &self.status_rx {
            if let Ok(version) = rx.try_recv() {
                self.server_version = version;
                self.status_rx = None;
            }
        }
        self.maybe_refresh_server_version();
        self.maybe_start_update_check();
        self.maybe_fetch_panel_status();
        let busy = self.busy();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("fafcn-sync");
                ui.label(egui::RichText::new(crate::BUILD_TAG).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = match self.lang {
                        GuiLang::Zh => "EN",
                        GuiLang::En => "中文",
                    };
                    if ui.button(label).clicked() {
                        self.lang = match self.lang {
                            GuiLang::Zh => GuiLang::En,
                            GuiLang::En => GuiLang::Zh,
                        };
                    }
                    // Manual update check: startup only checks once, so let
                    // the user re-check without restarting the app. One click
                    // checks all three updatable components: the sync client
                    // build plus the two server-side upstream sources.
                    let check_running = self.update_rx.is_some()
                        || self.check_rx.is_some()
                        || matches!(self.update, SelfUpdate::Checking);
                    let can_check =
                        !check_running && !self.busy() && !self.server.trim().is_empty();
                    if ui
                        .add_enabled(
                            can_check,
                            egui::Button::new(tr(self.lang, Txt::UpdateCheckNow)).small(),
                        )
                        .clicked()
                    {
                        self.update_checked_server.clear();
                        self.update = SelfUpdate::Checking;
                        self.update_check_manual = true;
                        self.log.push(log_update_checking(self.lang));
                        self.start_manual_upstream_check();
                    }
                });
            });

            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Sync, tr(self.lang, Txt::TabSync));
                ui.selectable_value(
                    &mut self.tab,
                    Tab::UploadPatch,
                    tr(self.lang, Txt::TabUploadPatch),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::UploadClient,
                    tr(self.lang, Txt::TabUploadClient),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::UploadMaps,
                    tr(self.lang, Txt::TabUploadMaps),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Settings,
                    tr(self.lang, Txt::TabSettings),
                );
            });
            ui.separator();

            self.update_row(ui);
            ui.add_space(4.0);

            self.shared_fields(ui);

            if self.tab == Tab::Sync {
                ui.add_space(4.0);
                self.version_panel(ui);
                ui.add_space(6.0);
                // Optional large content, opt-in via checkboxes.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tr(self.lang, Txt::SyncContent)).strong());
                    ui.label(
                        egui::RichText::new(tr(self.lang, Txt::SyncContentHint))
                            .small()
                            .weak(),
                    );
                });
                ui.add_enabled_ui(!busy, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.sync_coop, tr(self.lang, Txt::ChannelCoop));
                        ui.checkbox(&mut self.sync_maps, tr(self.lang, Txt::ChannelMaps));
                    });
                });
            }

            if self.tab == Tab::UploadPatch {
                ui.add_space(4.0);
                let detected = self.detected_patch_version();
                ui.horizontal(|ui| {
                    ui.label(tr(self.lang, Txt::PatchVersionLabel));
                    match &detected {
                        Some(v) => {
                            ui.strong(v);
                            ui.label(
                                egui::RichText::new(tr(self.lang, Txt::VersionAuto))
                                    .small()
                                    .weak(),
                            );
                        }
                        None => {
                            ui.add_enabled_ui(!busy, |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.patch_version)
                                        .hint_text("3825")
                                        .desired_width(120.0),
                                );
                            });
                            ui.label(
                                egui::RichText::new(tr(self.lang, Txt::VersionUndetected))
                                    .small()
                                    .weak(),
                            );
                        }
                    }
                });
                // Map generator version (auto-detected; informational only).
                ui.horizontal(|ui| {
                    ui.label(tr(self.lang, Txt::GeneratorVersion));
                    match &self.detected_generator {
                        Some(v) => {
                            ui.strong(v);
                        }
                        None => {
                            ui.label(
                                egui::RichText::new(tr(self.lang, Txt::GeneratorMissing))
                                    .small()
                                    .weak(),
                            );
                        }
                    }
                });
                ui.label(
                    egui::RichText::new(tr(self.lang, Txt::UploadHint))
                        .small()
                        .weak(),
                );
            }

            if self.tab == Tab::UploadClient {
                ui.add_space(4.0);
                ui.label(tr(self.lang, Txt::FieldClientFile));
                ui.add_enabled_ui(!busy, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.client_file)
                            .hint_text(r"D:\Downloads\dfc_windows_1_6_3.exe")
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button(tr(self.lang, Txt::Browse)).clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                let name = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                if let Some(v) = fafcn_gamedata::detect_version_from_filename(&name)
                                {
                                    self.client_version = v;
                                }
                                self.client_file = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(tr(self.lang, Txt::VersionLabel));
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.client_version)
                                .hint_text("1.6.3")
                                .desired_width(120.0),
                        );
                    });
                });
            }
            if self.tab == Tab::UploadMaps {
                ui.add_space(4.0);
                ui.label(tr(self.lang, Txt::MapsDirLabel));
                ui.add_enabled_ui(!busy, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.maps_dir)
                            .hint_text(r"D:\MyMaps")
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button(tr(self.lang, Txt::Browse)).clicked() {
                            if let Some(path) = folder_picker(&self.maps_dir).pick_folder() {
                                self.maps_dir = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                });
                ui.label(
                    egui::RichText::new(tr(self.lang, Txt::MapsHint))
                        .small()
                        .weak(),
                );
            }
            ui.add_space(12.0);

            if self.tab != Tab::Settings {
                self.action_button(ui);
            }

            let (done, total) = self.progress;
            if total > 0 {
                let fraction = done as f32 / total as f32;
                let text = if self.scanning {
                    let (done_files, total_files) = self.progress_files;
                    format!(
                        "{} {}/{}  ·  {} / {}",
                        tr(self.lang, Txt::Hashing),
                        done_files,
                        total_files,
                        format_bytes(done),
                        format_bytes(total),
                    )
                } else {
                    format!(
                        "{:.0}%  ·  {} / {}  ·  {}",
                        fraction * 100.0,
                        format_bytes(done),
                        format_bytes(total),
                        format_speed(self.speed),
                    )
                };
                ui.add(egui::ProgressBar::new(fraction).text(text));
            }
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui
                    .button(egui::RichText::new(tr(self.lang, Txt::CopyLog)).small())
                    .on_hover_text("fafcn-sync-log.log")
                    .clicked()
                {
                    ui.ctx().copy_text(self.log.join("\n"));
                }
            });
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.label(line);
                    }
                });
        });
    }

    /// Persist the settings on window close. The tab-switch save alone is
    /// not enough: a user who edits the mirror address and closes the window
    /// directly from the Settings tab would otherwise lose the edit (and the
    /// embedded mirror address would greet them again on the next launch).
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.persisted_config().save();
    }
}
