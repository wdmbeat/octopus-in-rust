//! Sync channel definitions shared by server, client, and web.
//!
//! Players only struggle to download specific things, so the mirror is
//! organized into well-known channels:
//!
//! - `gamedata` — only the big patch archives (`env.nx2`, `units.nx2`,
//!   `textures.nx2`), versioned by the FAF patch version from `lua.nx2`,
//!   plus frozen legacy extras (`faforever.faf`) that are manual-upload-only
//!   and preserved across auto-updates. The full per-file rule table is
//!   [`GAMEDATA_FILES`].
//! - `map-generator` — every `MapGenerator_*.jar` in the newest few version
//!   series (`1.22.x`, `1.21.x`, `1.20.x`), versioned by the newest jar's
//!   version.
//! - `faf-client` — the client installer (mirror-only).
//! - `maps` — FAF maps (folders like `name.v0001`), synced into the FAF
//!   Client's `maps_and_mods/maps` folder instead of the FAForever folder.
//! - `coop` — co-op mission support files (`bin/init_coop.lua`,
//!   `gamedata/lobby_coop.cop`, `gamedata/*_VO.nx2` voice-overs), synced into
//!   the FAForever ROOT (manifest paths carry their `bin/`/`gamedata/`
//!   prefix). Manually uploaded; the auto-updater does not mirror it (the
//!   official coop file list is not anonymously visible).
//! - `bin` — the FAF-patched game binary (`ForgedAlliance.exe`) that the
//!   official client otherwise downloads from FAF's content server on first
//!   launch. Pre-seeding it via the mirror lets new players skip that
//!   download. NOTE: FAF deliberately distributes this exe only through
//!   their official client to ownership-verified accounts; see
//!   `docs/fafcn/game-binary-channel.md` before changing this channel.

use std::collections::HashSet;

/// Channel id for the gamedata patch archives.
pub const CHANNEL_GAMEDATA: &str = "gamedata";

/// Channel id for the map generator jars.
pub const CHANNEL_MAP_GENERATOR: &str = "map-generator";

/// Channel id for the FAF client installer (mirror-only: players download it
/// from the web page; it is NOT synced into the FAForever folder).
pub const CHANNEL_FAF_CLIENT: &str = "faf-client";

/// Channel id for FAF maps. Synced into the FAF Client's
/// `maps_and_mods/maps` folder, NOT the FAForever folder (hence absent from
/// [`SYNC_CHANNELS`]); uploads MERGE into the existing manifest.
pub const CHANNEL_MAPS: &str = "maps";

/// Channel id for co-op mission support files. Synced into the FAForever
/// ROOT (paths carry their own `bin/`/`gamedata/` prefix).
pub const CHANNEL_COOP: &str = "coop";

/// Channel id for the FAF-patched game binary (`ForgedAlliance.exe`).
/// Synced into the FAForever `bin/` folder.
pub const CHANNEL_BIN: &str = "bin";

/// File name of the FAF-patched game binary mirrored by the [`CHANNEL_BIN`]
/// channel (`bin/ForgedAlliance.exe` below the FAForever root). The official
/// client downloads it from FAF's content server on first launch.
pub const FORGED_ALLIANCE_EXE: &str = "ForgedAlliance.exe";

/// All known channel ids (rejected at the API boundary otherwise).
pub const CHANNELS: &[&str] = &[
    CHANNEL_GAMEDATA,
    CHANNEL_MAP_GENERATOR,
    CHANNEL_FAF_CLIENT,
    CHANNEL_MAPS,
    CHANNEL_COOP,
    CHANNEL_BIN,
];

/// Channels the sync client syncs into the FAForever folder.
pub const SYNC_CHANNELS: &[&str] = &[
    CHANNEL_GAMEDATA,
    CHANNEL_MAP_GENERATOR,
    CHANNEL_COOP,
    CHANNEL_BIN,
];

/// How a mirrored file is acquired and maintained — the shared vocabulary
/// for every channel's per-file sync rules. Every consumer (server
/// auto-updater, client upload planner) matches on this exhaustively, so
/// adding a new rule kind surfaces every site that must decide how to
/// handle it.
///
/// Note the boundary: this models FILE rules (acquisition + upload
/// semantics). CHANNEL lifecycle (sync target dir, prune policy,
/// merge-vs-replace commit, replaydata mirror, opt-in flags) stays
/// per-channel — see `channel_subdir` and `docs/fafcn/file-sync.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSyncRule {
    /// Auto-mirrored from the official patch CDN: the server auto-updater
    /// derives the upstream URL `{dir}.{version}.nx2`, so the name MUST end
    /// in `.nx2`. Required on manual upload (the upload bails when the file
    /// is missing locally).
    PatchArchive,
    /// Auto-mirrored from a GitHub latest-release asset (map-generator jar,
    /// faf-client installer). Versioned file names; the channel prunes
    /// superseded versions on commit.
    GithubReleaseAsset,
    /// No anonymous upstream (the official client fetches it from a
    /// Cloudflare-HMAC-gated URL via the OAuth API). Seeded by manual upload
    /// when present locally (optional), then carried forward by the server
    /// auto-updater across patches.
    ///
    /// Current member: `faforever.faf`, the pre-`.nx2` monolithic FAF mod
    /// archive, frozen since ~version 3634 (2019): FAF's "latest files"
    /// query takes `MAX(version)` per fileId, so this stale row stays in the
    /// file list forever and every client re-downloads it during game prep.
    /// Current `init_faf.lua` never mounts it (only whitelisted `*.nx2`), so
    /// it is inert — but mirroring it spares players a slow gated download.
    ManualPreserved,
    /// Manual upload only; the server auto-updater never touches it.
    ManualOnly,
}

impl FileSyncRule {
    /// Whether a manual upload bails when the file is missing locally.
    pub fn required_on_upload(self) -> bool {
        match self {
            Self::PatchArchive => true,
            Self::GithubReleaseAsset | Self::ManualPreserved | Self::ManualOnly => false,
        }
    }

    /// Whether the server auto-updater fetches it (vs. manual-upload-only).
    pub fn auto_fetched(self) -> bool {
        match self {
            Self::PatchArchive | Self::GithubReleaseAsset => true,
            Self::ManualPreserved | Self::ManualOnly => false,
        }
    }
}

/// How a file-table row matches local files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMatch {
    /// Exact path below the channel's sync root (fixed file name).
    Exact(&'static str),
    /// File name suffix, e.g. `"_VO.nx2"` (coop voice-over banks).
    Suffix(&'static str),
    /// Any `*.nx2` not in [`FAF_STANDARD_NX2`] (coop-specific archives).
    NonStandardNx2,
}

/// One mirrored file (or file pattern) and its acquisition rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncFile {
    pub file_match: FileMatch,
    pub rule: FileSyncRule,
}

impl SyncFile {
    /// The fixed file path for [`FileMatch::Exact`] rows, else `None`.
    pub fn exact_name(&self) -> Option<&'static str> {
        match self.file_match {
            FileMatch::Exact(name) => Some(name),
            _ => None,
        }
    }
}

/// The single source of truth for gamedata-channel files: adding a mirrored
/// file is one row here, and the [`FileSyncRule`] variant drives both the
/// server auto-updater and the client upload planner.
pub const GAMEDATA_FILES: &[SyncFile] = &[
    SyncFile {
        file_match: FileMatch::Exact("env.nx2"),
        rule: FileSyncRule::PatchArchive,
    },
    SyncFile {
        file_match: FileMatch::Exact("units.nx2"),
        rule: FileSyncRule::PatchArchive,
    },
    SyncFile {
        file_match: FileMatch::Exact("textures.nx2"),
        rule: FileSyncRule::PatchArchive,
    },
    SyncFile {
        file_match: FileMatch::Exact("faforever.faf"),
        rule: FileSyncRule::ManualPreserved,
    },
];

/// The `bin` channel's file table (synced into the FAForever `bin/` folder).
pub const BIN_FILES: &[SyncFile] = &[SyncFile {
    file_match: FileMatch::Exact(FORGED_ALLIANCE_EXE),
    rule: FileSyncRule::ManualOnly,
}];

/// The coop channel's file table (synced into the FAForever ROOT; the Exact
/// paths carry their `bin/`/`gamedata/` prefixes). `bin/init_coop.lua`
/// doubles as the channel gate: without it coop was never downloaded
/// locally and the upload planner skips the channel entirely.
pub const COOP_FILES: &[SyncFile] = &[
    SyncFile {
        file_match: FileMatch::Exact("bin/init_coop.lua"),
        rule: FileSyncRule::ManualOnly,
    },
    SyncFile {
        file_match: FileMatch::Exact("gamedata/lobby_coop.cop"),
        rule: FileSyncRule::ManualOnly,
    },
    SyncFile {
        file_match: FileMatch::Suffix("_VO.nx2"),
        rule: FileSyncRule::ManualOnly,
    },
    SyncFile {
        file_match: FileMatch::NonStandardNx2,
        rule: FileSyncRule::ManualOnly,
    },
];

/// File rule for channels whose file names are dynamic (no per-file table).
/// Returns `None` for channels with a per-file table instead
/// ([`GAMEDATA_FILES`], [`BIN_FILES`], [`COOP_FILES`]).
pub fn channel_file_rule(channel: &str) -> Option<FileSyncRule> {
    match channel {
        CHANNEL_MAP_GENERATOR | CHANNEL_FAF_CLIENT => Some(FileSyncRule::GithubReleaseAsset),
        CHANNEL_MAPS => Some(FileSyncRule::ManualOnly),
        _ => None,
    }
}

/// The complete set of archive names the base `faf` featured mod deploys to
/// `gamedata/` (all ten are re-packed on every FAF deploy). The coop upload
/// whitelist uses this to EXCLUDE base-game archives: coop-specific archives
/// are whatever `.nx2` remains.
pub const FAF_STANDARD_NX2: &[&str] = &[
    "effects.nx2",
    "env.nx2",
    "etc.nx2",
    "loc.nx2",
    "lua.nx2",
    "meshes.nx2",
    "projectiles.nx2",
    "schook.nx2",
    "textures.nx2",
    "units.nx2",
];

/// Subfolder (below the FAForever root) each channel syncs into.
/// Mirror-only channels (faf-client) return `None`; coop syncs into the
/// root itself (its manifest paths carry `bin/`/`gamedata/` prefixes).
pub fn channel_subdir(channel: &str) -> Option<&'static str> {
    match channel {
        CHANNEL_GAMEDATA => Some("gamedata"),
        CHANNEL_MAP_GENERATOR => Some("map_generator"),
        CHANNEL_COOP => Some(""),
        CHANNEL_BIN => Some("bin"),
        _ => None,
    }
}

/// Filename pattern of map generator jars, e.g. `MapGenerator_1.22.1.jar`.
pub const MAP_GENERATOR_JAR_PREFIX: &str = "MapGenerator_";

/// How many recent map generator version SERIES to keep (server and client).
/// Every jar within those series is kept: when the newest jar is `1.22.x`,
/// all of `1.22.x`, `1.21.x` and `1.20.x` are kept, however many jars each
/// series contains.
pub const MAP_GENERATOR_KEEP_SERIES: usize = 3;

/// Extract a dotted version from a file name, e.g. `dfc_windows_1_6_3.exe`
/// or `downlords-faf-client-1.6.3.exe` → `1.6.3`. Returns the first run of
/// digits separated by `.`/`_` containing at least two numeric parts.
///
/// New-style names like `faf_windows-x64_2026_7_1.exe` glue the arch token
/// (`x64`) onto the version run, so a leading `32`/`64`/`86` part is dropped
/// when the run has more than three parts.
pub fn detect_version_from_filename(file_name: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut current = String::new();
    let flush = |current: &mut String, best: &mut Option<String>| {
        let mut parts: Vec<&str> = current
            .split(['.', '_'])
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() > 3 && matches!(parts.first(), Some(&"32" | &"64" | &"86")) {
            parts.remove(0);
        }
        if parts.len() >= 2 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
            let candidate = parts.join(".");
            let better = matches!(
                (
                    &best,
                    compare_version_strings(&candidate, best.as_deref().unwrap_or("0"))
                ),
                (None, _) | (Some(_), Some(std::cmp::Ordering::Greater))
            );
            if better {
                *best = Some(candidate);
            }
        }
        current.clear();
    };
    for c in file_name.chars() {
        if c.is_ascii_digit() || c == '.' || c == '_' {
            current.push(c);
        } else {
            flush(&mut current, &mut best);
        }
    }
    flush(&mut current, &mut best);
    best
}

/// Extract the version string from a `MapGenerator_<version>.jar` file name.
pub fn map_generator_jar_version(file_name: &str) -> Option<String> {
    let rest = file_name.strip_prefix(MAP_GENERATOR_JAR_PREFIX)?;
    let version = rest.strip_suffix(".jar")?;
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(version.to_string())
}

/// The `major.minor` series of a generator jar version (`1.22.1` → `1.22`;
/// a version with fewer than two components maps to itself). Jars within one
/// series are kept and pruned together.
pub fn map_generator_series(version: &str) -> &str {
    match version.match_indices('.').nth(1) {
        Some((idx, _)) => &version[..idx],
        None => version,
    }
}

/// The newest `max_series` jar version series across `versions` — e.g. for
/// [`MAP_GENERATOR_KEEP_SERIES`] and jars up to `1.22.2`, the set
/// `{1.22, 1.21, 1.20}` (assuming those series exist in `versions`).
pub fn newest_jar_series<'a>(
    versions: impl IntoIterator<Item = &'a str>,
    max_series: usize,
) -> HashSet<String> {
    let mut series: Vec<&str> = versions.into_iter().map(map_generator_series).collect();
    series.sort_by(|a, b| compare_version_strings(b, a).unwrap_or(std::cmp::Ordering::Equal));
    series.dedup();
    series
        .into_iter()
        .take(max_series)
        .map(str::to_string)
        .collect()
}

/// Parse a FAF map folder name of the form `base.vNNNN` (e.g.
/// `my_map.v0001`) into `(base, version)`. Returns `None` for names that
/// don't follow the convention.
pub fn map_folder_version(folder_name: &str) -> Option<(&str, u32)> {
    let (base, version) = folder_name.rsplit_once(".v")?;
    if base.is_empty() || version.is_empty() || !version.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((base, version.parse().ok()?))
}

/// Today's date as `YYYY-MM-DD` — the display version for maps commits
/// (the maps channel merges uploads, so there is no single patch version).
pub fn today_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Extract the mod version from a FAF `mod_info.lua` body: the first
/// `version = <digits>` line. Tolerates whitespace and Lua `--` comments.
/// Used for both the faf deploy branch (gamedata patch version) and the
/// fa-coop repo (coop mod version).
pub fn parse_mod_info_version(body: &str) -> Option<String> {
    for line in body.lines() {
        // Strip Lua line comments, then require `version = <digits>`.
        let line = line.split_once("--").map_or(line, |(code, _)| code).trim();
        let Some(rest) = line.strip_prefix("version") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    None
}

/// Compare two version strings as dotted numeric tuples (`3837` > `3825`,
/// `1.22.10` > `1.22.1`). Returns `None` when either side is not numeric.
pub fn compare_version_strings(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let parse = |s: &str| -> Option<Vec<u64>> {
        s.trim()
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect()
    };
    let mut av = parse(a)?.into_iter();
    let mut bv = parse(b)?.into_iter();
    Some(loop {
        match (av.next(), bv.next()) {
            (None, None) => break std::cmp::Ordering::Equal,
            (None, Some(0)) => continue,
            (None, Some(_)) => break std::cmp::Ordering::Less,
            (Some(_), None) => break std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                std::cmp::Ordering::Equal => continue,
                other => break other,
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn version_from_filename() {
        assert_eq!(
            detect_version_from_filename("dfc_windows_1_6_3.exe").as_deref(),
            Some("1.6.3")
        );
        assert_eq!(
            detect_version_from_filename("downlords-faf-client-1.6.10.exe").as_deref(),
            Some("1.6.10")
        );
        assert_eq!(
            detect_version_from_filename("MapGenerator_1.22.1.jar").as_deref(),
            Some("1.22.1")
        );
        assert_eq!(detect_version_from_filename("faf-client.exe"), None);
        assert_eq!(
            detect_version_from_filename("faf_windows-x64_2026_7_1.exe").as_deref(),
            Some("2026.7.1")
        );
    }

    #[test]
    fn jar_version_parsing() {
        assert_eq!(
            map_generator_jar_version("MapGenerator_1.22.1.jar").as_deref(),
            Some("1.22.1")
        );
        assert_eq!(
            map_generator_jar_version("MapGenerator_2.0.jar").as_deref(),
            Some("2.0")
        );
        assert_eq!(map_generator_jar_version("other.jar"), None);
        assert_eq!(map_generator_jar_version("MapGenerator_beta.jar"), None);
    }

    #[test]
    fn jar_series_grouping() {
        assert_eq!(map_generator_series("1.22.1"), "1.22");
        assert_eq!(map_generator_series("2.0"), "2.0");
        assert_eq!(map_generator_series("3"), "3");
    }

    #[test]
    fn newest_jar_series_picks_latest_distinct_series() {
        // Several jars per series: the keep set counts SERIES, not jars.
        let versions = [
            "1.22.2", "1.22.1", "1.22.0", "1.21.5", "1.21.0", "1.20.3", "1.19.9",
        ];
        let keep = newest_jar_series(versions, MAP_GENERATOR_KEEP_SERIES);
        assert_eq!(keep.len(), 3);
        assert!(keep.contains("1.22"));
        assert!(keep.contains("1.21"));
        assert!(keep.contains("1.20"));
        assert!(!keep.contains("1.19"));
        // Fewer series than the keep count: keep everything.
        let keep = newest_jar_series(["1.22.0", "1.21.0"], MAP_GENERATOR_KEEP_SERIES);
        assert_eq!(keep.len(), 2);
    }

    #[test]
    fn map_folder_version_parsing() {
        assert_eq!(map_folder_version("my_map.v0001"), Some(("my_map", 1)));
        assert_eq!(map_folder_version("astro.v0012"), Some(("astro", 12)));
        assert_eq!(map_folder_version("a.b.v0002"), Some(("a.b", 2)));
        assert_eq!(map_folder_version("noversion"), None);
        assert_eq!(map_folder_version("map.v"), None);
        assert_eq!(map_folder_version("map.vxyz"), None);
        assert_eq!(map_folder_version(".v0001"), None);
    }

    #[test]
    fn parse_mod_info_version_works() {
        assert_eq!(
            parse_mod_info_version("name = \"FAF\"\nversion = 3838\n"),
            Some("3838".to_string())
        );
        assert_eq!(
            parse_mod_info_version("  version   =   66  -- bumped\n"),
            Some("66".to_string())
        );
        assert_eq!(parse_mod_info_version("version=66"), Some("66".to_string()));
        assert_eq!(parse_mod_info_version(""), None);
        assert_eq!(parse_mod_info_version("no version here"), None);
        assert_eq!(parse_mod_info_version("version = \"66\""), None);
        assert_eq!(parse_mod_info_version("version_number = 66"), None);
        assert_eq!(parse_mod_info_version("-- version = 66"), None);
        assert_eq!(parse_mod_info_version("version = "), None);
    }

    #[test]
    fn file_table_invariants() {
        for table in [GAMEDATA_FILES, BIN_FILES, COOP_FILES] {
            // No duplicate Exact names within one table.
            let mut names: Vec<&str> = table
                .iter()
                .filter_map(|f| match f.file_match {
                    FileMatch::Exact(name) => Some(name),
                    _ => None,
                })
                .collect();
            names.sort_unstable();
            names.dedup();
            let unique = names.len();
            let exact = table
                .iter()
                .filter(|f| matches!(f.file_match, FileMatch::Exact(_)))
                .count();
            assert_eq!(unique, exact, "duplicate Exact file names");
            // Patch archives must follow the `{dir}.{version}.nx2` upstream
            // naming the auto-updater derives URLs from.
            for file in table {
                if file.rule == FileSyncRule::PatchArchive {
                    let FileMatch::Exact(name) = file.file_match else {
                        panic!("PatchArchive rows must be Exact");
                    };
                    assert!(
                        name.ends_with(".nx2"),
                        "{name}: PatchArchive names must end in .nx2"
                    );
                }
            }
        }
        // The coop table must contain the init-file gate row.
        assert!(COOP_FILES
            .iter()
            .any(|f| f.file_match == FileMatch::Exact("bin/init_coop.lua")));
    }

    #[test]
    fn every_channel_has_a_file_rule_classification() {
        for channel in CHANNELS {
            let classified = channel_file_rule(channel).is_some();
            let tabled = match *channel {
                CHANNEL_GAMEDATA => !GAMEDATA_FILES.is_empty(),
                CHANNEL_BIN => !BIN_FILES.is_empty(),
                CHANNEL_COOP => !COOP_FILES.is_empty(),
                _ => false,
            };
            assert!(
                classified ^ tabled,
                "{channel}: exactly one of channel_file_rule / per-file table"
            );
        }
        // The classifier covers the dynamic channels.
        assert_eq!(
            channel_file_rule(CHANNEL_MAP_GENERATOR),
            Some(FileSyncRule::GithubReleaseAsset)
        );
        assert_eq!(
            channel_file_rule(CHANNEL_FAF_CLIENT),
            Some(FileSyncRule::GithubReleaseAsset)
        );
        assert_eq!(
            channel_file_rule(CHANNEL_MAPS),
            Some(FileSyncRule::ManualOnly)
        );
    }

    #[test]
    fn coop_channel_registration() {
        assert!(CHANNELS.contains(&CHANNEL_COOP));
        assert!(SYNC_CHANNELS.contains(&CHANNEL_COOP));
        assert_eq!(channel_subdir(CHANNEL_COOP), Some(""));
    }

    #[test]
    fn bin_channel_registration() {
        assert!(CHANNELS.contains(&CHANNEL_BIN));
        assert!(SYNC_CHANNELS.contains(&CHANNEL_BIN));
        assert_eq!(channel_subdir(CHANNEL_BIN), Some("bin"));
    }

    #[test]
    fn version_comparison() {
        assert_eq!(compare_version_strings("3837", "3825"), Some(Greater));
        assert_eq!(compare_version_strings("1.22.1", "1.22.0"), Some(Greater));
        assert_eq!(compare_version_strings("1.22.10", "1.22.1"), Some(Greater));
        assert_eq!(compare_version_strings("1.22", "1.22.0"), Some(Equal));
        assert_eq!(compare_version_strings("1.22.0", "1.22.1"), Some(Less));
        assert_eq!(compare_version_strings("abc", "1.0"), None);
    }
}
