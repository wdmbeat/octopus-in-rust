# The `bin` channel: mirroring the FAF game binary (ForgedAlliance.exe)

> Added 2026-08-31. This channel touches a sensitive area of FAF's official
> distribution policy — read this document fully before changing it.

## 1. Background: the last gap in the onboarding flow

On first game launch, the official FAF client downloads a **FAF-patched
`ForgedAlliance.exe`** from FAF's content server into
`C:\ProgramData\FAForever\bin\` (status bar: "Preparing game: Downloading
ForgedAlliance.exe"). It is produced by FAF's official patcher applying
community-maintained engine patches onto the retail exe. What makes it
different from the Steam original, and where each part lives publicly:

| Aspect | Detail | Source |
|---|---|---|
| **Multiplayer rework** | The retail game's multiplayer depended on GameSpy (long dead). The patches adapt the netcode to FAF's ICE adapter (WebRTC) and raise the player cap from 8 to 12. | [`FAForever/FA-Binary-Patches`](https://github.com/FAForever/FA-Binary-Patches) (see `section/gpg_net.cpp`); the ICE adapter itself is a separate program, [`FAForever/faf-pioneer`](https://github.com/FAForever/faf-pioneer) |
| **Engine fixes** | Crash fixes and performance optimizations from a decade of community reverse-engineering after official support ended. | [`FAForever/FA-Binary-Patches`](https://github.com/FAForever/FA-Binary-Patches) (see its changelog) |
| **New Lua API** | New script functions injected into the engine (`SimSetCommandSource`, `GetTableSize`, `CopyToClipBoard`, icon scaling, …). FAF's mods and game logic depend on them — without this exe, FAF gameplay does not run. | [`FAForever/FA-Binary-Patches`](https://github.com/FAForever/FA-Binary-Patches) (`section/*.cpp`, e.g. `SimSetCommandSource.cpp`, `GetTableSize.cpp`) |
| **Versioning** | One exe per game patch version (e.g. `ForgedAlliance.3713.exe`); the client downloads the matching build per version. | Distribution mechanism (`content.faforever.com/legacy-featured-mod-files/...`), not a repo |
| **The patcher itself** | Tool that compiles the patches into the retail exe (`FaP.exe`). | [`FAForever/FA_Patcher`](https://github.com/FAForever/FA_Patcher); a Python rewrite also exists: [`FAForever/fa-python-binary-patcher`](https://github.com/FAForever/fa-python-binary-patcher) |

In other words: **"how to build it" is fully open source; the built artifact
deliberately is not** (see section 2).

Without this file, a new player following our guide still faces one slow
FAF-server download at the very last step. The `bin` channel closes the only
remaining gap in the mirror (gamedata patches, map generator, client
installer, maps): once fafcn-sync pre-seeds the exe, the FAF client finds it
present and **skips its own download**.

## 2. FAF's distribution policy (must read)

The `FA-Binary-Patches` README states explicitly:

> "Due to piracy concerns we can at no point upload the executable as an
> artifact. **We do not want the lua-compatible executable available to the
> public without verification that they own the game.** The compatible
> executable is distributed via the Official client. Before a user can use
> such end points his or her account needs to be verified."

I.e. the patch sources and the patcher are fully public, but **the finished
exe is deliberately not published** — it is only distributed through the
official client to accounts verified to own the game (Steam/GOG link),
because exe + the full gamedata set approaches a runnable copy of the game.

### Our position and decision

- Decision (2026-08-31): **implement anonymous mirroring first**, because:
  - The source principle is unchanged: the uploader's own exe, originally
    fetched by the official client under their ownership-verified account —
    the same principle as every other mirrored file.
  - FAF's own content server serves this exe over anonymous URLs in practice
    (the official client fetches it without credentials); FAF's ownership
    check lives at the account layer, not the download layer.
- However, we acknowledge the tension with FAF's stated intent, and this
  channel is directly relevant to the ownership-check question Sheikah
  raised (`faf-integration.md` §2.2). Therefore:
  - **We must disclose this channel proactively in the next FAF
    communication** (as part of the §2.2 reply), stating that we can gate it
    behind FAF OAuth login at any time if they prefer.
  - Once OAuth credentials arrive, prefer gating the `bin` channel behind
    login — turning §2.2 from "an open question" into "an implemented
    ownership check".
  - If FAF objects, taking the channel offline means deleting its manifest
    (server-side `data/faf-gamedata/channels/bin/`); clients without the exe
    behave exactly as older versions.

## 3. Technical implementation

Reuses the existing channel machinery; no server-specific code:

- Channel definition: `crates/fafcn-gamedata/src/channels.rs`
  (`CHANNEL_BIN = "bin"`, registered in `CHANNELS` / `SYNC_CHANNELS`,
  `channel_subdir → "bin"`).
- Download mounts: `apps/fafcn-server/src/routes.rs` derives every channel's
  static `files/` mount from `CHANNELS` in a loop. **Pitfall (hit on
  2026-09-02):** the mounts used to be a hand-maintained list — `bin` was
  missing, requests fell through to the SPA fallback, and players downloaded
  `index.html` instead of the exe (sha256 mismatch after 3 retries →
  "下载失败,已跳过"). The manifest/upload APIs validate the channel
  dynamically, so only downloads broke — always test the full download path
  when adding a channel, not just the manifest.
- Upload side: `apps/fafcn-sync/src/upload.rs ~plan_channels` — the
  上传补丁 (upload-patch) flow automatically includes the bin channel when
  `%ProgramData%\FAForever\bin\ForgedAlliance.exe` exists locally (hash
  phase shows live progress); **versioned by the same patch version as
  gamedata** (the exe tracks the game version).
- Sync side: no new code — the `SYNC_CHANNELS` loop downloads the exe into
  `FAForever/bin/ForgedAlliance.exe`.
- Web sync page: `channel_title` maps it to a localized label
  ("游戏主程序 (ForgedAlliance.exe)").

### First publish (ops steps)

1. `cargo xtask fafcn majiko-deploy` (the server must be rebuilt to accept
   the new channel name)
2. `cargo xtask fafcn majiko-deploy-file-sync` (player client)
3. The uploader updates fafcn-sync and runs one 上传补丁 upload → the bin
   channel goes live
4. Verify:

   ```bash
   curl -s https://faforever.cn:60/api/gamedata/channels/bin/manifest.json
   # expect: patch_version matching gamedata, files containing ForgedAlliance.exe
   ```

## 4. Resulting first-run experience

After guide step 4 (fafcn-sync) completes: gamedata patches ✅, map
generator ✅, maps ✅, `bin/ForgedAlliance.exe` ✅ — the FAF client's
first launch has no "Preparing game" download wait at all.
