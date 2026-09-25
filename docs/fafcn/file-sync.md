# fafcn File-Sync — Architecture & Developer Guide

Scope: everything about the fafcn **file-sync** feature — what it is, how each
piece works, and how to extend it safely. Read this before adding a channel,
changing sync rules, or touching the auto-updater.

Audience: developers. Player-facing instructions live on the `/sync` web page.

---

## 1. What problem it solves

Chinese FAF players often cannot download certain files (game patch archives,
map generator, the FAF client installer, maps) from official servers. The old
workaround was passing files through QQ: one VPN-having player downloads and
re-uploads everything, and everyone else manually figures out which files they
need.

File-sync replaces QQ with a mirror:

- **Server** (`apps/fafcn-server`) hosts the files plus a JSON manifest per
  **channel**, and can fetch new official patches **by itself**.
- **Client** (`apps/fafcn-sync`, a single Windows `.exe`) diffs the player's
  local folders against the manifests and downloads exactly what is missing —
  one button, no per-file decisions. The same exe also lets a trusted uploader
  publish files manually (secondary backup path).
- **Web** (`apps/fafcn-web`, `/sync` page) shows mirror status and distributes
  the client with the mirror address embedded.

## 2. Components at a glance

| Component             | Path                               | Role                                                                                                                                                  |
| --------------------- | ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shared protocol crate | `crates/fafcn-gamedata`            | Channel ids, manifest types, hashing, path validation, client-binary overlay.**Server, client, and web all depend on it so formats can never drift.** |
| Server                | `apps/fafcn-server`                | Axum HTTP API, on-disk store (`handlers/gamedata/store.rs`), auto-updater (`updater.rs`).                                                             |
| Sync client           | `apps/fafcn-sync`                  | eframe GUI (default, double-click) + CLI subcommands (`sync`, `upload`, `upload-maps`, `upload-client`).                                              |
| Web page              | `apps/fafcn-web/src/views/sync.rs` | Status display + client download link.                                                                                                                |

## 3. Core concepts

### 3.1 Channels

Everything the mirror serves belongs to a well-known channel
(`crates/fafcn-gamedata/src/channels.rs`):

```rust
// crates/fafcn-gamedata/src/channels.rs ~line 57 — channel registry
pub const CHANNELS: &[&str] = &[
    CHANNEL_GAMEDATA,      // "gamedata": env/units/textures.nx2 patch archives + frozen static extras (faforever.faf)
    CHANNEL_MAP_GENERATOR, // "map-generator": all MapGenerator_*.jar in the newest 3 version series
    CHANNEL_FAF_CLIENT,    // "faf-client": installer, mirror-only (not synced)
    CHANNEL_MAPS,          // "maps": FAF maps, merged uploads
    CHANNEL_COOP,          // "coop": co-op mission files, synced to FAForever root
    CHANNEL_BIN,           // "bin": FAF-patched ForgedAlliance.exe (policy: see game-binary-channel.md)
];

// Channels the sync client syncs into the FAForever folder (~line 67):
pub const SYNC_CHANNELS: &[&str] = &[
    CHANNEL_GAMEDATA,
    CHANNEL_MAP_GENERATOR,
    CHANNEL_COOP,
    CHANNEL_BIN,
];
```

Per-channel rules that new code MUST respect:

| Channel         | Synced into                                                          | Version source                                                                    | Client deletes?                        | Server file lifecycle                                                                                                     |
| --------------- | -------------------------------------------------------------------- | --------------------------------------------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `gamedata`      | `FAForever/gamedata` (+ mirrored to `FAForever/replaydata/gamedata`) | FAF patch version (`lua.nx2` on manual upload; `mod_info.lua` upstream)           | **Never** deletes extras               | Fixed filenames → overwritten in place; per-file rules live in the `GAMEDATA_FILES` table: `PatchArchive` entries are auto-mirrored, `ManualPreserved` extras (`faforever.faf`) are manual-upload-only and **preserved by the auto-updater** |
| `map-generator` | `FAForever/map_generator`                                            | Newest jar version (manual upload); auto-mirror uses the GitHub release tag       | Prunes jars beyond the newest 3 version series (`1.22.x`–`1.20.x` when 1.22 is newest) | Prune-on-commit: files not in the new manifest are deleted; also auto-mirrored from GitHub releases by the server updater |
| `faf-client`    | — (web download)                                                     | From installer filename on manual upload; auto-mirror uses the GitHub release tag | —                                      | Prune-on-commit; also auto-mirrored from GitHub releases by the server updater                                            |
| `coop` | FAForever **root** (paths carry `bin/`/`gamedata/` prefixes) | fa-coop `mod_info.lua` version, fetched from GitHub at upload time | Never deletes extras | Fixed names → overwritten in place; **manual upload only** (see TODO below) |
| `bin` | `FAForever/bin` | Same as the gamedata patch version (exe tracks the game version) | Never deletes extras | Fixed name → overwritten in place; manual upload via the 上传补丁 flow when `bin/ForgedAlliance.exe` exists locally; **policy: read `game-binary-channel.md`** |
| `maps` | FAF Client's `maps_and_mods/maps` | Date stamp (no single version) | Prunes superseded `name.vNNNN` versions | `commit_merge`: replaced map versions deleted, others kept |

**Why the coop channel touches `bin/`:** a coop game boots from the coop init
file, and the featured-mod system installs init files to `bin/`
(`bin/init_coop.lua`; the official client copies it to `bin/init.lua` and
launches `ForgedAlliance.exe /init init.lua`). The init file in turn assumes
that location — it does `dofile(InitFileDir .. '/../fa_path.lua')` and mounts
`InitFileDir .. '/../gamedata/'`, i.e. it must sit exactly one level below the
FAForever root. So the coop channel must reproduce a two-folder layout
(`bin/` + `gamedata/`), which is why it syncs into the root with
prefix-carrying manifest paths instead of a single subfolder.

**Per-file rules.** Within a channel, HOW each file is acquired and
maintained is declared as data in `channels.rs`: the `FileSyncRule` enum
(`PatchArchive` / `GithubReleaseAsset` / `ManualPreserved` / `ManualOnly`)
plus the per-channel tables `GAMEDATA_FILES`, `BIN_FILES`, `COOP_FILES`
(rows of `SyncFile { file_match, rule }`, where `FileMatch` covers exact
names and patterns like `Suffix("_VO.nx2")`). Channels with dynamic file
names are classified by `channel_file_rule()`. See §8 for how to extend.
The auto-updater and the upload planner both consume these tables, so a new
file is one row and a new rule kind fails compilation until every consumer
handles it.

### 3.2 Manifest — the single source of truth

```rust
// crates/fafcn-gamedata/src/manifest.rs ~line 22 — Manifest
pub struct Manifest {
    pub patch_version: String,
    pub uploader: String,
    pub generated_at: DateTime<Utc>,
    pub files: Vec<FileEntry>, // { path, size, sha256 }
}
```

One `manifest.json` per channel, regenerated atomically (write `.tmp` +
rename) on every accepted commit. Clients never decide anything beyond
"local file hash ≠ manifest hash → download".

### 3.3 Versions

`compare_version_strings` (`channels.rs ~line 355`) compares dotted-numeric
versions (`1.22.10 > 1.22.1`, `3838 > 3837`). The server's commit path
**rejects strictly older versions with 409** — the authoritative downgrade
guard. Never compare versions with plain string ordering.

## 4. Server: storage and file lifecycle

Layout under `FAFCN_GAMEDATA_DIR` (default `data/faf-gamedata/`):

```text
data/faf-gamedata/
  channels/<channel>/
    manifest.json   # generated, never hand-edited
    files/<path>    # served to clients (static, range-supported)
    incoming/       # temp dir; files are renamed into files/ atomically
  client/           # sync client binaries + VERSION build tag
```

File lifecycle per channel (all in
`apps/fafcn-server/src/handlers/gamedata/store.rs`):

- **Store one file** — `store_upload` (bytes, ~line 110) for the upload API;
  `store_file_from_path` (~line 143) for the auto-updater (streams: `env.nx2`
  is ~500 MB). Both: validate relative path → hash-verify → write
  `incoming/<uuid>.part` → atomic `rename` into `files/`. Same-name files are
  replaced in place, which is why gamedata never accumulates.
- **`commit()`** (~line 174) — validate all files present with matching
  hashes → downgrade guard → atomic manifest write → **prune-on-commit** for
  `map-generator` / `faf-client`: anything in `files/` not in the new manifest
  is deleted (`prune_unlisted_files`, ~line 338) so versioned jars/installers
  don't accumulate. `gamedata` is explicitly excluded.
- **`commit_merge()`** (~line 201, maps only) — merges instead of replaces:
  a map whose base name is re-uploaded has ALL its older versions replaced and
  deleted from disk; unrelated maps are kept.
- **Startup maps audit** (`audit_maps_channel`, called once from `main.rs` at
  boot) — drops any map version whose files are not all present on disk with
  the manifest's size, and deletes its leftover partial folder. A map with
  missing files is unusable, so advertising it only makes every client's sync
  fail on it. Existence + size check only (no hashing — startup must stay
  fast); no-op when nothing is missing.

## 5. The three data flows

### 5.1 Sync flow (mirror → player)

`sync_gamedata` (`apps/fafcn-sync/src/sync.rs ~line 404`), shared by GUI and
CLI:

1. `prepare_upstream` (~line 281) — **asks the server to check upstream
   first** (see 5.2) and waits while the server is downloading a component
   the sync consumes — the gamedata patch **and the map generator jars**
   (5 s polls, 10 min cap, one wait per component); the mirror-only FAF
   client installer is never waited on. Best-effort: any error → log and
   continue.
2. Per `SYNC_CHANNELS`: fetch manifest → diff local files by sha256 →
   download missing/mismatched to a `.fafcn-sync-tmp` dir → verify → atomic
   rename into place.
3. Per-channel cleanup: gamedata reports (never deletes) extra files and
   **mirrors the manifest files into `FAForever/replaydata/gamedata`**
   (`mirror_to_replaydata`, `sync.rs`) — the FAF client reads patch files
   from that separate copy when playing replays and would otherwise download
   mismatches from the official servers. Mirroring is local-only (hash-check
   → copy → atomic rename) and never overwrites a working replaydata copy
   with a bad gamedata one; map-generator prunes jars beyond the newest 3
   version series (but never a jar the manifest tracks — that would just
   re-download it on the next sync).
   `sync_maps` handles the maps channel separately (different root folder).

Progress flows through the `SyncProgress` enum (including the `Upstream`
phase) so GUI and CLI render it their own way.

### 5.2 Auto-update flow (official FAF → mirror)

`apps/fafcn-server/src/updater.rs`. One update pass covers three independent
upstream sources (`run_update`, ~line 318 — when one phase fails the others
still run, and the first error lands in `last_error`):

1. **gamedata patch** (`update_gamedata`, ~line 341). The official FAF
   client detects patches via an OAuth-gated API + HMAC-signed downloads; we
   replicate the result anonymously:
   - Version: `https://raw.githubusercontent.com/FAForever/fa/deploy/faf/mod_info.lua`
     → `version = NNNN` (the exact integer FAF's deployment pipeline stamps).
   - Files: `https://content.faforever.com/faf/updaterNew/updates_faf_files/{dir}.{version}.nx2`
     (legacy path, no token; all dirs re-packed on every deploy).
   - Static extras (`FileSyncRule::ManualPreserved`, e.g. `faforever.faf`)
     are NOT
     fetched — no anonymous upstream exists (the official client gets them
     from the HMAC-gated `legacy-featured-mod-files/` path via the OAuth
     API). They are seeded by manual upload and carried forward: before
     committing a new patch, `update_gamedata` re-appends the previous
     manifest's `ManualPreserved` entries (safe because the gamedata channel
     never prunes on-disk files).
   - **`faforever.faf` background:** the old monolithic FAF mod archive from
     the pre-`.nx2` era (frozen since ~v3634, 2019). FAF's "latest files"
     query (`LegacyFeaturedModFileRepository.getFiles` in faf-java-api)
     takes `MAX(version)` **per fileId**, so this stale row stays in the
     featured-mod file list forever and every official client re-downloads
     it into `gamedata/` during game prep. The current deployment pipeline
     (`LegacyFeaturedModDeploymentTask`) never regenerates it, and current
     `init_faf.lua` never mounts it (whitelisted `*.nx2` only) — inert dead
     weight, but mirroring it removes a slow gated download for players.
2. **FAF client installer** (`update_faf_client`, ~line 439). Latest GitHub
   release of `FAForever/downlords-faf-client`
   (`CLIENT_RELEASE_API`, ~line 56; the API requires a `User-Agent` header).
   The version comes from the **tag** (`v2026.7.1` → `2026.7.1`), NOT the
   file name — `detect_version_from_filename("faf_windows-x64_2026_7_1.exe")`
   would misread the `x64` run as digits. Asset pick (`parse_client_release`,
   ~line 100): prefer the `faf_windows*.exe` installer, fall back to the old
   `dfc_windows_*.exe` naming; error when the release has no Windows
   installer. The newest release is recorded in
   `UpdaterInfo.latest_client_version`.
3. **Map generator jars** (`update_map_generator`, ~line 568). The GitHub
   release LIST of `FAForever/Neroxis-Map-Generator`
   (`GENERATOR_RELEASES_API`, ~line 62) — the same endpoint the official
   client polls when the user opens the "generate map" dialog; its download
   URL format is `releases/download/{version}/NeroxisGen_{version}.jar`.
   Parsing (`parse_generator_releases`, ~line 133): exact
   `NeroxisGen_<version>.jar` per release, falling back to any
   `NeroxisGen_*.jar`; non-numeric tags (prereleases) and jar-less releases
   are skipped. Not just the latest release is mirrored: EVERY release in
   the newest 3 version series (`1.22.x`, `1.21.x`, `1.20.x` when 1.22 is
   newest) is wanted, because a player joining a game on a map generated
   with an older generator needs that exact jar — missing ones are
   backfilled. Each jar is stored under the channel's
   `MapGenerator_<version>.jar` name; the commit lists the downloaded jars
   plus the existing entries in the kept series, and prune-on-commit drops
   what falls off. Newest release recorded in
   `UpdaterInfo.latest_generator_version`.

Two triggers share one `update_once` (~line 306) behind a single-flight mutex

- 30 s debounce (`trigger`, ~line 285):

1. **Poller** — `spawn_poller` (~line 769), hardcoded 24 h interval, first
   tick on boot. All knobs are constants (`POLL_INTERVAL`, `VERSION_URL`,
   `BASE_URL`, ~line 53) — no env vars.
2. **Manual** — `POST /api/gamedata/upstream/refresh` (anonymous), called by
   the client at sync start and by the GUI's 检查更新 button. Returns an
   `UpdaterInfo` snapshot immediately; the download runs in the background.

One gamedata pass: parse version → compare with the gamedata manifest (equal
→ done; mirror newer → skip, never downgrade) → state
`Downloading{component: Gamedata, version}` (the component distinguishes it
from the faf-client download so clients never wait on the wrong one) →
stream the 3 archives to `incoming/`, verify Content-Length + sha256 →
`store_file_from_path` as unversioned names → `commit()` with
`uploader: "auto-updater"`. The faf-client pass works the same way for its
single installer asset; the map-generator pass downloads every missing jar
in the keep range (as `Downloading{component: FafClient|MapGenerator, ..}`);
prune-on-commit deletes the superseded installer and jars of dropped series
automatically. Any failure (including the deploy-lag 404 race)
cleans temp files, records `last_error`, returns to `Idle`. The updater never
panics and never blocks the server.

Upstream HTTP sits behind an injectable `UpstreamFetch` trait (~line 152) so
tests need no network.

### 5.2.1 TODO — co-op auto-mirror (blocked)

**Goal:** a fourth auto-updater phase (`update_coop`) so the coop channel
self-updates like gamedata/faf-client/map-generator — no manual upload.

**Why the gamedata trick does not transfer.** Auto-mirroring needs two
anonymous things: (1) a machine-readable *version source* and (2) *derivable
file URLs*. For gamedata both exist (`mod_info.lua` on the deploy branch +
`updaterNew/updates_faf_files/{dir}.{version}.nx2`). For coop, (1) exists but
(2) does not: the deployed coop files live ONLY at
`content.faforever.com/legacy-featured-mod-files/updates_coop_files/`, which is
Cloudflare-HMAC-gated (**403 anonymous**, verified) — coop never got a legacy
`updaterNew` mirror (**404**, verified). So unlike gamedata, we cannot just
download the official artifacts; we would have to **repack them ourselves**
from the public sources.

**What we have in hand (all anonymous, verified):**

| Need | Source | Status |
|---|---|---|
| Coop mod version | `raw.githubusercontent.com/FAForever/fa-coop/master/mod_info.lua` → `version = 66` | ✅ |
| Mod content (`init_coop.lua`, `mods/`, `units/`) | `codeload.github.com/FAForever/fa-coop/tar.gz/refs/heads/master` | ✅ |
| Voice-overs `A01_VO.nx2`…(the bulk of the bytes) | GitHub release assets of `fa-coop` tag `v49` (frozen since v49) | ✅ |
| Mission map zips | `content.faforever.com/maps/{folder}.zip` | ✅ (but no anonymous version *listing* — they ride the `maps` channel) |

**What we are missing (the blocker):** the official **packaging contract** —
the exact set of `{group, name}` the deployed files use, i.e. the contents of
the `updates_coop_files` DB table. We must not guess it, because:

- The packing rule (`LegacyFeaturedModDeploymentTask`) zips each top-level
  repo dir to `{dir}.{version}.nx2` → installed as `gamedata/{dir}.nx2`.
  fa-coop has a `units/` dir, which would produce `gamedata/units.nx2` and
  **collide with the base faf `units.nx2`** owned by the gamedata channel.
  Real installs don't break this way, so the official names must differ —
  but we can't see how.
- If our manifest names differ from what the official client installs, the
  client will just re-download from the official servers anyway (md5
  mismatch) — the mirror would be useless, or worse, shadow a real file.

**How to unblock (one-time, ~15 min):** the deployed structure is frozen —
see it once, hardcode the mapping forever. Either:

1. **Authenticated API dump (preferred):** with any FAF account OAuth token,
   run `GET https://api.faforever.com/data/featuredMod?filter=technicalName=="coop"`
   to get the mod id, then `GET /featuredMods/{id}/files/latest` — the
   response IS the definitive `{group, name, md5, version}` list. Paste it
   into this doc / the issue.
2. **Working-install diff:** a player whose coop works lists
   `C:\ProgramData\FAForever\bin` and `…\gamedata` (everything beyond the 10
   standard `FAF_STANDARD_NX2` archives + `init_coop.lua` is coop's).

**After unblocking, the implementation is mechanical:** new updater phase
`update_coop` — poll `mod_info.lua` → on bump, download repo tarball + v49
VO assets → pack per the verified mapping → commit to the `coop` channel as
`uploader: "auto-updater"` (prune stays off; fixed names overwrite in place).
Add `UpdaterComponent::Coop` + `latest_coop_version`, one panel row, done.

**Until then** the channel is manual-upload-only: `plan_coop`
(`apps/fafcn-sync/src/upload.rs`) sweeps a working install according to the
`COOP_FILES` table (`channels.rs`): the `bin/init_coop.lua` gate row plus
`gamedata/lobby_coop.cop` (Exact), `*_VO.nx2` (Suffix), and non-standard
`.nx2` archives (NonStandardNx2) — correct by construction, since it mirrors
exactly what the official client produced on a real machine.

### 5.3 Manual upload flow (uploader → mirror, backup path)

`apps/fafcn-sync/src/upload.rs` (GUI 上传 tab / `upload` CLI):

1. `POST .../upload/check` with the full `{path, size, sha256}` list → server
   replies which files it still needs (dedup).
2. `POST .../upload/file` per needed file — raw body +
   `x-gamedata-path` (percent-encoded) / `x-gamedata-sha256` headers.
3. `POST .../upload/commit` → server verifies everything, enforces the
   downgrade guard (409), regenerates the manifest.

Auth: a single shared bearer token (`FAFCN_GAMEDATA_UPLOAD_TOKEN`) — the
uploaders are trusted because they effectively patch everyone's game. Patch
version is auto-detected from `lua.nx2` on the uploader's machine
(`apps/fafcn-sync/src/version.rs`); the GUI pre-disables upload when the
server is already newer, and the commit-time 409 is the authoritative
enforcement. Which local files are collected per channel is driven by the
`GAMEDATA_FILES` / `BIN_FILES` / `COOP_FILES` tables and their
`FileSyncRule::required_on_upload()` (see §3.1): required files bail the
upload when missing, optional files are included when present.

### 5.4 Client distribution with embedded config

`GET /api/gamedata/client/<file>` patches the exe per request: a JSON config
(mirror origin from `X-Forwarded-Proto` + `Host`) is appended as PE/ELF
overlay data (`crates/fafcn-gamedata/src/overlay.rs`), so a player's client
starts with 镜像地址 pre-filled. On the **first run of a new build**
(`ClientConfig.last_build_tag` differs from the running `BUILD_TAG`), the
embedded origin replaces the remembered address — this repairs stale
addresses left by old builds (e.g. a retired domain) without hardcoding any
URL; afterwards the remembered address wins again, so user edits stick.
Release binaries are published by `cargo xtask fafcn file-sync`.

The client self-updates (`apps/fafcn-sync/src/update.rs` + `gui/self_update.rs`):
it compares its `BUILD_TAG` against the mirror's `StatusResponse.client_tag`
at startup, when the mirror address changes, or when the user clicks the
**检查更新** button in the top bar; a newer build is downloaded next to the
running exe and swapped in via rename-and-relaunch (Windows cannot overwrite
a running exe, but can rename it).

**CJK fonts:** egui's bundled fonts have no Chinese glyphs. The GUI prefers
the OS CJK font (YaHei/SimHei/…) and falls back to an embedded subsetted
Noto Sans SC (`apps/fafcn-sync/assets/cjk-fallback.ttf`, ASCII + GB2312,
SIL OFL — license in the same dir) so Chinese renders even on Windows
installs without CJK supplemental fonts (`gui/fonts.rs`).

The **检查更新** button checks all three updatable components at once: the
sync-client build above, plus the three server-side upstream sources — it POSTs
the debounced `upstream/refresh`, then polls `/api/gamedata/status` every 2 s
(max ~15 s) until the check finishes, and **keeps polling while the server is
downloading** (every 5 s, max ~10 min), streaming live snapshots so the
panel's blue "server downloading" row shows; the conclusion lines (gamedata
patch, FAF client, map generator) then reflect the final state
(`apps/fafcn-sync/src/gui/version_panel.rs`).

GUI layout: the sync tab keeps the update row, a **version panel** (one
freshness row per component — sync client, gamedata patch, FAF client, map
generator — from `/api/gamedata/status`), the big 开始同步 button and the
log. 镜像地址, the FAForever folder and the FAF Client folder live on the
dedicated **设置** tab (persisted when leaving it); the sync tab shows only a
compact warning pointing there when the FAForever folder is invalid.

## 6. HTTP API reference

All under `/api/gamedata`; channel ids validated against `CHANNELS`.

| Method & path                       | Auth          | Purpose                                                  |
| ----------------------------------- | ------------- | -------------------------------------------------------- |
| `GET /channels/<ch>/manifest.json`  | —             | Read the channel manifest                                |
| `GET /channels/<ch>/files/<path>`   | —             | Static download, HTTP range support                      |
| `GET /status`                       | —             | Per-channel summary + client build tag +`updater` status |
| `POST /upstream/refresh`            | — (debounced) | Trigger an upstream check; returns`UpdaterInfo`          |
| `POST /channels/<ch>/upload/check`  | Bearer        | Which of the listed files the server still needs         |
| `POST /channels/<ch>/upload/file`   | Bearer        | Store one file (raw body + path/sha256 headers)          |
| `POST /channels/<ch>/upload/commit` | Bearer        | Verify + publish manifest (`commit_merge` for maps)      |
| `GET /client/<file>`                | —             | Sync client binary with embedded mirror config           |

## 7. Invariants — read before changing anything

- **Never delete a player's files implicitly.** Client-side, gamedata extras
  are reported, never removed; pruning is limited to map-generator jars beyond
  the newest few version series (jars the manifest tracks are never pruned)
  and superseded map versions.
- **Atomicity everywhere.** Temp file in `incoming/` → hash-verify → rename.
  A failed download/upload/commit leaves the previous state fully intact.
- **The manifest is the only source of truth**, and it is only ever replaced
  atomically at commit time.
- **Never downgrade.** `compare_version_strings` + the commit-time 409 guard;
  the auto-updater additionally skips when the mirror is ahead.
- **Typed enums for states** (`UpdaterState`, `UpstreamEvent`,
  `SyncProgress`), typed serde structs for the wire (shared crate only) — per
  root `AGENTS.md`. No `serde_json::Value` field-picking.
- **Best-effort upstream check.** A player must always be able to sync
  whatever the mirror has, even if upstream/GitHub is unreachable.
- **Server-side pruning is channel-aware**: prune-on-commit is ONLY safe for
  channels whose manifest lists every file that should exist
  (map-generator, faf-client). Never enable it for gamedata.
- **Updater phases are independent**: a failing GitHub release fetch must
  never block the gamedata phase (and vice versa); the first error is
  recorded in `last_error`.

## 8. How to extend

### Add a new channel

1. `crates/fafcn-gamedata/src/channels.rs`: add `CHANNEL_X`, register in
   `CHANNELS`; add to `SYNC_CHANNELS` + `channel_subdir` if it syncs into the
   FAForever folder. Classify its file rule: a per-file table (like
   `GAMEDATA_FILES`/`BIN_FILES`/`COOP_FILES`) for fixed names/patterns, or a
   `channel_file_rule()` arm for dynamic names —
   `every_channel_has_a_file_rule_classification` fails otherwise.
2. Decide the **server file lifecycle**: fixed names (nothing to do),
   versioned names (add to the prune-on-commit match in
   `store.rs::commit`), or merge semantics (route to `commit_merge` in
   `handlers/gamedata/mod.rs::upload_commit`).
3. Mount `files/` in `apps/fafcn-server/src/routes.rs` (ServeDir).
4. Client: sync rules in `apps/fafcn-sync/src/sync.rs` (per-channel match in
   `sync_gamedata`), upload planning in `upload.rs::plan_channels`, GUI
   strings in `gui/strings.rs`.
5. Web: label/i18n in `apps/fafcn-web/src/i18n.rs`, display in
   `views/sync.rs`.
6. Tests: store roundtrip + prune/no-prune behavior; client diff logic.

### Change the mirrored gamedata files

Per-file sync rules across ALL channels share one vocabulary in
`crates/fafcn-gamedata/src/channels.rs` (~line 85):

- `FileSyncRule` — how a file is acquired/maintained: `PatchArchive`
  (auto-mirrored from the FAF patch CDN, name must end in `.nx2`, required
  on manual upload), `GithubReleaseAsset` (auto-mirrored from a GitHub
  release; map-generator/faf-client), `ManualPreserved` (no upstream; manual
  seed + auto-updater preserves it, e.g. `faforever.faf`), `ManualOnly`
  (manual upload only). `required_on_upload()`/`auto_fetched()` are full
  matches, so a new variant forces a decision at the definition site.
- `FileMatch` — how a table row matches local files: `Exact(name)`,
  `Suffix("_VO.nx2")`, `NonStandardNx2`.
- Per-channel tables: `GAMEDATA_FILES`, `BIN_FILES`, `COOP_FILES` (rows of
  `SyncFile { file_match, rule }`). Channels with dynamic file names are
  classified by `channel_file_rule()` instead (map-generator/faf-client →
  `GithubReleaseAsset`, maps → `ManualOnly`).

So: add a mirrored file = one table row; add a rule kind = one
`FileSyncRule` variant (the compiler then lists every consumer that must
handle it); add a file pattern = one `FileMatch` variant. Channel LIFECYCLE
(sync target, prune policy, merge-vs-replace, replaydata mirror) is a
separate concept and stays per-channel — do not force it into the enum.

### Change auto-updater behavior

Constants at the top of `apps/fafcn-server/src/updater.rs`. Keep upstream
access behind `UpstreamFetch` and the state machine inside
`update_once` — triggers (poller/endpoint/future ones) must stay thin.

## 9. Testing

- All store logic is unit-tested against temp dirs (`temp_store()` pattern in
  `store.rs`): roundtrip, downgrade 409, merge semantics, path traversal,
  prune/no-prune.
- The updater is tested end-to-end with a counting `FakeFetch`: full
  download→commit (both channels), skip-when-current, client-release newer →
  commit + old installer pruned, 404→`Idle`+`last_error`, GitHub release
  fetch failure leaving the gamedata phase unaffected, debounce. Release-JSON
  parsing (asset pick, tag→version) is unit-tested from sample payloads.
- No test touches the network. Run: `cargo test -p fafcn-gamedata -p fafcn-server -p fafcn-sync`.
- Live smoke: with the mirror behind upstream by one patch, start the server
  — the poller's first tick should commit the new patch with
  `uploader: "auto-updater"`.
