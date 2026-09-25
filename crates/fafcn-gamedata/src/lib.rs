//! Shared manifest and API types for the FAF gamedata sync service.
//!
//! Both `fafcn-server` (HTTP API) and `fafcn-sync` (CLI client) depend on this
//! crate so the manifest format and upload protocol can never drift apart.

mod channels;
mod error;
mod hash;
mod manifest;
mod overlay;
mod path;

pub use channels::{
    channel_file_rule, channel_subdir, compare_version_strings, detect_version_from_filename,
    map_folder_version, map_generator_jar_version, map_generator_series, newest_jar_series,
    parse_mod_info_version, today_stamp, FileMatch, FileSyncRule, SyncFile, BIN_FILES, CHANNELS,
    CHANNEL_BIN, CHANNEL_COOP, CHANNEL_FAF_CLIENT, CHANNEL_GAMEDATA, CHANNEL_MAPS,
    CHANNEL_MAP_GENERATOR, COOP_FILES, FAF_STANDARD_NX2, FORGED_ALLIANCE_EXE, GAMEDATA_FILES,
    MAP_GENERATOR_JAR_PREFIX, MAP_GENERATOR_KEEP_SERIES, SYNC_CHANNELS,
};
pub use error::{Error, Result};
pub use hash::{sha256_bytes, sha256_file, sha256_file_with_progress};
pub use manifest::{
    ChannelStatus, FileEntry, Manifest, ManifestSummary, StatusResponse, UpdaterComponent,
    UpdaterInfo, UpdaterState, UploadCheckRequest, UploadCheckResponse, UploadCommitRequest,
};
pub use overlay::{append_config, read_config, EmbeddedConfig};
pub use path::{decode_relative_path, encode_relative_path, validate_relative_path};
