//! Translatable GUI strings and log lines (Zh default, En fallback).

/// UI language (target audience is Chinese players, so Zh is the default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GuiLang {
    Zh,
    En,
}

impl GuiLang {
    pub(super) fn from_config(value: Option<&str>) -> Self {
        match value {
            Some("en") => GuiLang::En,
            _ => GuiLang::Zh,
        }
    }

    pub(super) fn code(self) -> &'static str {
        match self {
            GuiLang::Zh => "zh",
            GuiLang::En => "en",
        }
    }
}

/// Translatable GUI strings.
#[derive(Debug, Clone, Copy)]
pub(super) enum Txt {
    TabSync,
    TabUploadPatch,
    TabUploadClient,
    ServerLabel,
    DirLabel,
    Browse,
    Detect,
    DirValid,
    DirSuspicious,
    DirMissing,
    SyncNow,
    Syncing,
    Hashing,
    IdleHint,
    TokenLabel,
    PatchVersionLabel,
    UploaderLabel,
    UploadNow,
    Uploading,
    UploadHint,
    MissingPrefix,
    FieldServer,
    FieldDir,
    FieldToken,
    FieldPatchVersion,
    FieldUploader,
    VersionAuto,
    VersionUndetected,
    GeneratorVersion,
    GeneratorMissing,
    ChannelGamedata,
    ChannelMapGenerator,
    ChannelFafClient,
    UploadClientNow,
    VersionLabel,
    FieldClientFile,
    TabUploadMaps,
    TabSettings,
    ChannelMaps,
    ChannelCoop,
    ChannelBin,
    MapsDirLabel,
    MapsHint,
    FieldMapsDir,
    FafClientDirLabel,
    FafClientFound,
    FafClientMissing,
    FafClientDirMissing,
    CopyLog,
    UpdateChecking,
    UpdateCheckNow,
    UpdateUpToDate,
    UpdateNow,
    UpdateRetry,
    UpdateFailed,
    UpdateDownloading,
    UpdateRestarting,
    PanelTitle,
    PanelSyncClient,
    PanelGamedata,
    VersionUnknown,
    ServerDownloading,
    PanelMirror,
    PanelOfficial,
    DirInvalidOpenSettings,
    UpstreamStatusUnknown,
    SyncContent,
    SyncContentHint,
}

pub(super) fn tr(lang: GuiLang, txt: Txt) -> &'static str {
    match (txt, lang) {
        (Txt::TabSync, GuiLang::Zh) => "同步",
        (Txt::TabSync, GuiLang::En) => "Sync",
        (Txt::TabUploadPatch, GuiLang::Zh) => "上传补丁",
        (Txt::TabUploadPatch, GuiLang::En) => "Upload patch",
        (Txt::TabUploadClient, GuiLang::Zh) => "上传客户端",
        (Txt::TabUploadClient, GuiLang::En) => "Upload client",
        (Txt::ServerLabel, GuiLang::Zh) => "镜像地址",
        (Txt::ServerLabel, GuiLang::En) => "Mirror address",
        (Txt::DirLabel, GuiLang::Zh) => "FAForever 目录",
        (Txt::DirLabel, GuiLang::En) => "FAForever folder",
        (Txt::Browse, GuiLang::Zh) => "浏览…",
        (Txt::Browse, GuiLang::En) => "Browse…",
        (Txt::Detect, GuiLang::Zh) => "自动检测",
        (Txt::Detect, GuiLang::En) => "Auto-detect",
        (Txt::DirValid, GuiLang::Zh) => "有效的 FAForever 目录",
        (Txt::DirValid, GuiLang::En) => "Valid FAForever folder",
        (Txt::DirSuspicious, GuiLang::Zh) => {
            "注意:该目录看起来不像 FAForever 目录(应包含 gamedata 子目录及 .nx2 文件)"
        }
        (Txt::DirSuspicious, GuiLang::En) => {
            "Warning: this doesn't look like the FAForever folder (expected a gamedata subfolder containing .nx2 files)"
        }
        (Txt::DirMissing, GuiLang::Zh) => "错误:目录不存在",
        (Txt::DirMissing, GuiLang::En) => "Error: folder does not exist",
        (Txt::SyncNow, GuiLang::Zh) => "开始同步",
        (Txt::SyncNow, GuiLang::En) => "Sync now",
        (Txt::Syncing, GuiLang::Zh) => "正在同步…",
        (Txt::Syncing, GuiLang::En) => "Syncing…",
        (Txt::Hashing, GuiLang::Zh) => "正在计算本地文件校验和…",
        (Txt::Hashing, GuiLang::En) => "Hashing local files…",
        (Txt::IdleHint, GuiLang::Zh) => "确认镜像地址和目录后,点击“开始同步”。",
        (Txt::IdleHint, GuiLang::En) => "Check the mirror address and folder, then click \"Sync now\".",
        (Txt::TokenLabel, GuiLang::Zh) => "上传令牌",
        (Txt::TokenLabel, GuiLang::En) => "Upload token",
        (Txt::PatchVersionLabel, GuiLang::Zh) => "补丁版本",
        (Txt::PatchVersionLabel, GuiLang::En) => "Patch version",
        (Txt::UploaderLabel, GuiLang::Zh) => "玩家",
        (Txt::UploaderLabel, GuiLang::En) => "Player",
        (Txt::UploadNow, GuiLang::Zh) => "开始上传",
        (Txt::UploadNow, GuiLang::En) => "Start upload",
        (Txt::Uploading, GuiLang::Zh) => "正在上传…",
        (Txt::Uploading, GuiLang::En) => "Uploading…",
        (Txt::UploadHint, GuiLang::Zh) => {
            "仅供有 VPN 的上传者使用:先从官方渠道下载最新补丁到 gamedata 目录,再在此处上传。令牌请向服务器部署者索取。"
        }
        (Txt::UploadHint, GuiLang::En) => {
            "For VPN-having uploaders only: download the latest patch into your gamedata folder via the official channel first, then upload it here. Ask the server admin for the token."
        }
        (Txt::MissingPrefix, GuiLang::Zh) => "还需要填写:",
        (Txt::MissingPrefix, GuiLang::En) => "Still needed: ",
        (Txt::FieldServer, GuiLang::Zh) => "镜像地址",
        (Txt::FieldServer, GuiLang::En) => "mirror address",
        (Txt::FieldDir, GuiLang::Zh) => "FAForever 目录",
        (Txt::FieldDir, GuiLang::En) => "FAForever folder",
        (Txt::FieldToken, GuiLang::Zh) => "上传令牌",
        (Txt::FieldToken, GuiLang::En) => "upload token",
        (Txt::FieldPatchVersion, GuiLang::Zh) => "补丁版本",
        (Txt::FieldPatchVersion, GuiLang::En) => "patch version",
        (Txt::FieldUploader, GuiLang::Zh) => "玩家",
        (Txt::FieldUploader, GuiLang::En) => "player",
        (Txt::VersionAuto, GuiLang::Zh) => "(从 lua.nx2 自动检测)",
        (Txt::VersionAuto, GuiLang::En) => "(auto-detected from lua.nx2)",
        (Txt::VersionUndetected, GuiLang::Zh) => "无法自动检测,请手动填写",
        (Txt::VersionUndetected, GuiLang::En) => "could not auto-detect; enter manually",
        (Txt::GeneratorVersion, GuiLang::Zh) => "地图生成器",
        (Txt::GeneratorVersion, GuiLang::En) => "map generator",
        (Txt::GeneratorMissing, GuiLang::Zh) => "未安装,上传时跳过",
        (Txt::GeneratorMissing, GuiLang::En) => "not installed; skipped on upload",
        (Txt::ChannelGamedata, GuiLang::Zh) => "游戏数据",
        (Txt::ChannelGamedata, GuiLang::En) => "gamedata",
        (Txt::ChannelMapGenerator, GuiLang::Zh) => "地图生成器",
        (Txt::ChannelMapGenerator, GuiLang::En) => "map-generator",
        (Txt::ChannelFafClient, GuiLang::Zh) => "FAF 客户端",
        (Txt::ChannelFafClient, GuiLang::En) => "FAF client",
        (Txt::UploadClientNow, GuiLang::Zh) => "上传安装包",
        (Txt::UploadClientNow, GuiLang::En) => "Upload installer",
        (Txt::VersionLabel, GuiLang::Zh) => "版本",
        (Txt::VersionLabel, GuiLang::En) => "Version",
        (Txt::FieldClientFile, GuiLang::Zh) => "安装包文件",
        (Txt::FieldClientFile, GuiLang::En) => "installer file",
        (Txt::TabUploadMaps, GuiLang::Zh) => "上传地图",
        (Txt::TabUploadMaps, GuiLang::En) => "Upload maps",
        (Txt::TabSettings, GuiLang::Zh) => "设置",
        (Txt::TabSettings, GuiLang::En) => "Settings",
        (Txt::ChannelMaps, GuiLang::Zh) => "地图",
        (Txt::ChannelMaps, GuiLang::En) => "maps",
        (Txt::ChannelCoop, GuiLang::Zh) => "合作任务",
        (Txt::ChannelCoop, GuiLang::En) => "co-op missions",
        (Txt::ChannelBin, GuiLang::Zh) => "游戏主程序",
        (Txt::ChannelBin, GuiLang::En) => "game binary",
        (Txt::MapsDirLabel, GuiLang::Zh) => "地图文件夹",
        (Txt::MapsDirLabel, GuiLang::En) => "Maps folder",
        (Txt::MapsHint, GuiLang::Zh) => {
            "选择包含地图的文件夹:其中的所有地图(包括子文件夹内的文件)都会上传到镜像。上传是合并式的:同名地图的旧版本会被你上传的版本替换,其他人上传的地图不受影响。"
        }
        (Txt::MapsHint, GuiLang::En) => {
            "Pick a folder of maps: every map inside (including files in subfolders) is uploaded to the mirror. Uploads are merged: your versions replace older versions of the same maps; maps uploaded by others are kept."
        }
        (Txt::FieldMapsDir, GuiLang::Zh) => "地图文件夹",
        (Txt::FieldMapsDir, GuiLang::En) => "maps folder",
        (Txt::FafClientDirLabel, GuiLang::Zh) => "FAF Client 目录(含 faf-client.exe)",
        (Txt::FafClientDirLabel, GuiLang::En) => "FAF Client folder (contains faf-client.exe)",
        (Txt::FafClientFound, GuiLang::Zh) => "已找到 faf-client.exe,地图将同步到 maps_and_mods/maps",
        (Txt::FafClientFound, GuiLang::En) => {
            "faf-client.exe found — maps sync into maps_and_mods/maps"
        }
        (Txt::FafClientMissing, GuiLang::Zh) => "目录存在但没有 faf-client.exe,将跳过地图同步",
        (Txt::FafClientMissing, GuiLang::En) => {
            "folder exists but has no faf-client.exe — maps sync will be skipped"
        }
        (Txt::FafClientDirMissing, GuiLang::Zh) => "目录不存在,将跳过地图同步",
        (Txt::FafClientDirMissing, GuiLang::En) => "folder does not exist — maps sync will be skipped",
        (Txt::CopyLog, GuiLang::Zh) => "复制日志",
        (Txt::CopyLog, GuiLang::En) => "Copy log",
        (Txt::UpdateChecking, GuiLang::Zh) => "正在检查更新…",
        (Txt::UpdateChecking, GuiLang::En) => "Checking for updates…",
        (Txt::UpdateCheckNow, GuiLang::Zh) => "检查更新",
        (Txt::UpdateCheckNow, GuiLang::En) => "Check update",
        (Txt::UpdateUpToDate, GuiLang::Zh) => "已是最新",
        (Txt::UpdateUpToDate, GuiLang::En) => "Up to date",
        (Txt::UpdateNow, GuiLang::Zh) => "立即更新",
        (Txt::UpdateNow, GuiLang::En) => "Update now",
        (Txt::UpdateRetry, GuiLang::Zh) => "重试",
        (Txt::UpdateRetry, GuiLang::En) => "Retry",
        (Txt::UpdateFailed, GuiLang::Zh) => "更新失败",
        (Txt::UpdateFailed, GuiLang::En) => "Update failed",
        (Txt::UpdateDownloading, GuiLang::Zh) => "正在更新…",
        (Txt::UpdateDownloading, GuiLang::En) => "Updating…",
        (Txt::UpdateRestarting, GuiLang::Zh) => "更新完成,正在重启…",
        (Txt::UpdateRestarting, GuiLang::En) => "Update installed, restarting…",
        (Txt::PanelTitle, GuiLang::Zh) => "版本状态",
        (Txt::PanelTitle, GuiLang::En) => "Version status",
        (Txt::PanelSyncClient, GuiLang::Zh) => "fafcn-sync 客户端",
        (Txt::PanelSyncClient, GuiLang::En) => "fafcn-sync client",
        (Txt::PanelGamedata, GuiLang::Zh) => "gamedata 补丁",
        (Txt::PanelGamedata, GuiLang::En) => "gamedata patch",
        (Txt::VersionUnknown, GuiLang::Zh) => "未知",
        (Txt::VersionUnknown, GuiLang::En) => "unknown",
        (Txt::ServerDownloading, GuiLang::Zh) => "服务器下载中",
        (Txt::ServerDownloading, GuiLang::En) => "server downloading",
        (Txt::PanelMirror, GuiLang::Zh) => "镜像",
        (Txt::PanelMirror, GuiLang::En) => "mirror",
        (Txt::PanelOfficial, GuiLang::Zh) => "官方",
        (Txt::PanelOfficial, GuiLang::En) => "official",
        (Txt::DirInvalidOpenSettings, GuiLang::Zh) => "FAForever 目录无效,请到“设置”页修改",
        (Txt::DirInvalidOpenSettings, GuiLang::En) => {
            "Invalid FAForever folder — fix it on the Settings tab"
        }
        (Txt::UpstreamStatusUnknown, GuiLang::Zh) => "服务器未返回更新状态",
        (Txt::UpstreamStatusUnknown, GuiLang::En) => "server returned no updater status",
        (Txt::SyncContent, GuiLang::Zh) => "勾选以同步额外内容",
        (Txt::SyncContent, GuiLang::En) => "Sync optional content",
        (Txt::SyncContentHint, GuiLang::Zh) => "体积较大，不勾选也能正常游戏",
        (Txt::SyncContentHint, GuiLang::En) => "large downloads; not needed for regular games",
    }
}

pub(super) fn txt_update_available(lang: GuiLang, tag: &str) -> String {
    match lang {
        GuiLang::Zh => format!("发现新版本:{tag}"),
        GuiLang::En => format!("New version available: {tag}"),
    }
}

/// Localized display name for a channel id.
pub(super) fn channel_name(lang: GuiLang, channel: &str) -> &'static str {
    match channel {
        fafcn_gamedata::CHANNEL_MAP_GENERATOR => tr(lang, Txt::ChannelMapGenerator),
        fafcn_gamedata::CHANNEL_FAF_CLIENT => tr(lang, Txt::ChannelFafClient),
        fafcn_gamedata::CHANNEL_MAPS => tr(lang, Txt::ChannelMaps),
        fafcn_gamedata::CHANNEL_COOP => tr(lang, Txt::ChannelCoop),
        fafcn_gamedata::CHANNEL_BIN => tr(lang, Txt::ChannelBin),
        _ => tr(lang, Txt::ChannelGamedata),
    }
}

pub(super) fn log_channel_started(lang: GuiLang, channel: &str) -> String {
    format!("—— {} ——", channel_name(lang, channel))
}

pub(super) fn log_channel_empty(lang: GuiLang, channel: &str) -> String {
    match lang {
        GuiLang::Zh => format!("镜像还没有{},请上传者先发布", channel_name(lang, channel)),
        GuiLang::En => format!(
            "mirror has no {} yet — ask an uploader to publish it",
            channel_name(lang, channel)
        ),
    }
}

pub(super) fn log_manifest(
    lang: GuiLang,
    channel: &str,
    patch: &str,
    files: usize,
    mb: f64,
    uploader: &str,
) -> String {
    match lang {
        GuiLang::Zh => format!(
            "{}版本 {patch}({files} 个文件,{mb:.1} MB),由 {uploader} 上传",
            channel_name(lang, channel)
        ),
        GuiLang::En => format!(
            "{} {patch} ({files} files, {mb:.1} MB), uploaded by {uploader}",
            channel_name(lang, channel)
        ),
    }
}

pub(super) fn log_plan(lang: GuiLang, downloads: usize, mb: f64) -> String {
    match (lang, downloads) {
        (_, 0) => match lang {
            GuiLang::Zh => "已是最新,无需下载。".to_string(),
            GuiLang::En => "up to date — nothing to download.".to_string(),
        },
        (GuiLang::Zh, _) => format!("需要下载 {downloads} 个文件,共 {mb:.1} MB"),
        (GuiLang::En, _) => format!("Downloading {downloads} file(s), {mb:.1} MB total"),
    }
}

pub(super) fn log_file(lang: GuiLang, index: usize, count: usize, path: &str) -> String {
    match lang {
        GuiLang::Zh => format!("[{index}/{count}] 已安装 {path}"),
        GuiLang::En => format!("[{index}/{count}] installed {path}"),
    }
}

pub(super) fn log_file_failed(
    lang: GuiLang,
    index: usize,
    count: usize,
    path: &str,
    err: &str,
) -> String {
    match lang {
        GuiLang::Zh => format!("[{index}/{count}] 下载失败,已跳过 {path}:{err}"),
        GuiLang::En => format!("[{index}/{count}] download failed, skipped {path}: {err}"),
    }
}

pub(super) fn log_update_checking(lang: GuiLang) -> String {
    tr(lang, Txt::UpdateChecking).to_string()
}

pub(super) fn log_update_up_to_date(lang: GuiLang) -> String {
    match lang {
        GuiLang::Zh => format!("已是最新版本({})", crate::BUILD_TAG),
        GuiLang::En => format!("Already up to date ({})", crate::BUILD_TAG),
    }
}

pub(super) fn log_pruned(lang: GuiLang, path: &str) -> String {
    match lang {
        GuiLang::Zh => format!("已清理旧版本:{path}"),
        GuiLang::En => format!("pruned old version: {path}"),
    }
}

pub(super) fn log_mirrored(lang: GuiLang, path: &str) -> String {
    match lang {
        GuiLang::Zh => format!("已同步到回放目录:{path}"),
        GuiLang::En => format!("mirrored to replaydata: {path}"),
    }
}

pub(super) fn log_sync_done(lang: GuiLang, files: usize) -> String {
    match lang {
        GuiLang::Zh => {
            if files == 0 {
                "完成:无需更改。".to_string()
            } else {
                format!("同步完成,共更新 {files} 个文件。可以启动 FAF 客户端了!")
            }
        }
        GuiLang::En => {
            if files == 0 {
                "Done: no changes needed.".to_string()
            } else {
                format!("Sync complete — {files} file(s) updated. You can start FAF now!")
            }
        }
    }
}

pub(super) fn log_scanned(lang: GuiLang, files: usize, mb: f64) -> String {
    match lang {
        GuiLang::Zh => format!("本地共 {files} 个文件,{mb:.1} MB"),
        GuiLang::En => format!("Found {files} file(s), {mb:.1} MB total"),
    }
}

pub(super) fn log_needed(lang: GuiLang, needed: usize) -> String {
    match (lang, needed) {
        (_, 0) => match lang {
            GuiLang::Zh => "服务器已有全部文件,无需上传。".to_string(),
            GuiLang::En => "Server already has every file — nothing to upload.".to_string(),
        },
        (GuiLang::Zh, _) => format!("服务器需要 {needed} 个文件,开始上传…"),
        (GuiLang::En, _) => format!("Server needs {needed} file(s), uploading…"),
    }
}

pub(super) fn log_uploaded_file(lang: GuiLang, index: usize, count: usize, path: &str) -> String {
    match lang {
        GuiLang::Zh => format!("[{index}/{count}] 已上传 {path}"),
        GuiLang::En => format!("[{index}/{count}] uploaded {path}"),
    }
}

pub(super) fn log_committed(lang: GuiLang, channel: &str, files: usize) -> String {
    match lang {
        GuiLang::Zh => format!("{}清单已提交({files} 个文件)", channel_name(lang, channel)),
        GuiLang::En => {
            format!(
                "{} manifest committed ({files} files)",
                channel_name(lang, channel)
            )
        }
    }
}

pub(super) fn log_channel_skipped(lang: GuiLang, channel: &str, reason: &str) -> String {
    match lang {
        GuiLang::Zh => format!("跳过{}:{reason}", channel_name(lang, channel)),
        GuiLang::En => format!("skipping {}: {reason}", channel_name(lang, channel)),
    }
}

pub(super) fn log_upload_done(
    lang: GuiLang,
    published: &[crate::upload::PublishedChannel],
) -> String {
    // The faf-client installer is downloaded from the website, not synced —
    // point players there instead of at the sync tool.
    if published
        .iter()
        .all(|p| p.channel == fafcn_gamedata::CHANNEL_FAF_CLIENT)
    {
        let version = published.first().map(|p| p.version.as_str()).unwrap_or("");
        return match lang {
            GuiLang::Zh => format!(
                "上传完成,FAF 客户端 {version} 已发布!玩家们现在可以在网站的补丁同步页下载它。"
            ),
            GuiLang::En => format!(
                "Upload complete — FAF client {version} is live! Players can now download it from the sync page."
            ),
        };
    }
    let list = published
        .iter()
        .map(|p| format!("{} {}", p.channel, p.version))
        .collect::<Vec<_>>()
        .join(", ");
    match lang {
        GuiLang::Zh => format!("上传完成,已发布:{list},所有人现在都可以同步了!"),
        GuiLang::En => {
            format!("Upload complete — published: {list}. It's live for everyone to sync!")
        }
    }
}

pub(super) fn log_maps_skipped(lang: GuiLang) -> String {
    match lang {
        GuiLang::Zh => "未设置有效的 FAF Client 目录,跳过地图同步。".to_string(),
        GuiLang::En => "FAF Client folder not set — skipping maps sync.".to_string(),
    }
}

pub(super) fn log_failed(lang: GuiLang, err: &str) -> String {
    match lang {
        GuiLang::Zh => format!("操作失败:{err}"),
        GuiLang::En => format!("Failed: {err}"),
    }
}

pub(super) fn txt_server_newer(lang: GuiLang, server: &str, local: &str) -> String {
    match lang {
        GuiLang::Zh => format!("服务器已有更新的补丁 {server}(你的是 {local}),无需上传"),
        GuiLang::En => {
            format!("Server already has newer patch {server} (yours is {local}); nothing to upload")
        }
    }
}

pub(super) fn log_upstream_checking(lang: GuiLang) -> String {
    match lang {
        GuiLang::Zh => "正在检查官方补丁…".to_string(),
        GuiLang::En => "Checking for a new official patch…".to_string(),
    }
}

pub(super) fn log_upstream_downloading(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("服务器正在从官方下载补丁 v{version},请稍候…"),
        GuiLang::En => format!("Server is downloading official patch v{version}, please wait…"),
    }
}

pub(super) fn log_upstream_up_to_date(lang: GuiLang) -> String {
    match lang {
        GuiLang::Zh => "服务器已是最新".to_string(),
        GuiLang::En => "Server is up to date".to_string(),
    }
}

pub(super) fn log_upstream_timeout(lang: GuiLang, version: Option<&str>) -> String {
    match lang {
        GuiLang::Zh => format!(
            "等待官方补丁{}超时,将同步镜像当前版本",
            version.map(|v| format!(" v{v}")).unwrap_or_default()
        ),
        GuiLang::En => format!(
            "Timed out waiting for upstream patch{}; syncing what the mirror has",
            version.map(|v| format!(" v{v}")).unwrap_or_default()
        ),
    }
}

pub(super) fn log_upstream_skipped(lang: GuiLang, reason: &str) -> String {
    match lang {
        GuiLang::Zh => format!("官方补丁检查失败,不影响同步:{reason}"),
        GuiLang::En => format!("Upstream check skipped (sync continues): {reason}"),
    }
}

/// The server is downloading a new map generator jar; the sync waits for it.
pub(super) fn log_generator_downloading(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("服务器正在下载地图生成器 v{version},请稍候…"),
        GuiLang::En => format!("Server is downloading map generator v{version}, please wait…"),
    }
}

/// Waiting for the server's map generator download timed out.
pub(super) fn log_generator_timeout(lang: GuiLang, version: Option<&str>) -> String {
    match lang {
        GuiLang::Zh => format!(
            "等待地图生成器{}下载超时,将同步镜像当前版本",
            version.map(|v| format!(" v{v}")).unwrap_or_default()
        ),
        GuiLang::En => format!(
            "Timed out waiting for map generator{}; syncing what the mirror has",
            version.map(|v| format!(" v{v}")).unwrap_or_default()
        ),
    }
}

/// Conclusion of a manual gamedata upstream check: mirror already current.
pub(super) fn log_gamedata_current(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("官方补丁 v{version},镜像已是最新"),
        GuiLang::En => format!("Official patch v{version}, mirror is up to date"),
    }
}

/// Conclusion of a manual faf-client upstream check: mirror already current.
pub(super) fn log_client_current(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("FAF 客户端已是最新({version})"),
        GuiLang::En => format!("FAF client is up to date ({version})"),
    }
}

/// Conclusion of a manual faf-client upstream check: newer release exists.
pub(super) fn log_client_new(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("FAF 客户端有新版本 v{version},服务器下载中"),
        GuiLang::En => format!("New FAF client v{version}, server is downloading"),
    }
}

/// Conclusion of a manual map-generator upstream check: mirror already current.
pub(super) fn log_generator_current(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("地图生成器已是最新(v{version})"),
        GuiLang::En => format!("Map generator is up to date (v{version})"),
    }
}

/// Conclusion of a manual map-generator upstream check: newer release exists.
pub(super) fn log_generator_new(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("地图生成器有新版本 v{version},服务器下载中"),
        GuiLang::En => format!("New map generator v{version}, server is downloading"),
    }
}

/// Version-panel status: mirror holds the newest version.
pub(super) fn txt_current_version(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("已是最新(v{version})"),
        GuiLang::En => format!("up to date (v{version})"),
    }
}

/// Version-panel status: upstream has a newer version.
pub(super) fn txt_new_version(lang: GuiLang, version: &str) -> String {
    match lang {
        GuiLang::Zh => format!("有新版本 v{version}"),
        GuiLang::En => format!("new version v{version}"),
    }
}
