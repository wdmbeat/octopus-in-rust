# FAF-ML: Unit Detection from Game Screenshots — Handover Doc

> **Read this first when you're back.** Goal of the project: point a model at
> a Forged Alliance (FAF) screenshot and get a per-player unit breakdown
> (the "analysis" view). All code lives in the **octopus** workspace; the
> separate `dive_into_deep_learning_in_rust` workspace is the d2l.ai learning
> track (chapters 2–10 ported; chapters 11+ not started).

## The one-paragraph version of the whole approach

Manual labeling doesn't scale, so we don't do it: `faf-ml-datagen` (driven
from the server's Datagen view) pastes the
game's own strategic-icon sprites onto crops of **empty-terrain screenshots**,
which yields perfectly labeled synthetic training data for free. A small SSD
detector (`crates/faf-ml-model`) trains on that. Real **battle** screenshots
are never trained on — they're the held-out exam. If the model fails on real
screenshots, fix the *generator* (colors/scale), not the labels. Later, the
model pre-labels real shots and you correct them in the web UI — that
correction set closes the domain gap.

## What's built and verified (as of 2026-09-10)

| Piece | Where | State |
|---|---|---|
| Synthetic data generator | `crates/faf-ml-datagen` (runs as a server job; the `faf-datagen` CLI is gone) | ✅ works; tint + scale + clustering; streams samples into the store as `synthetic` screenshots with JSON labels |
| Web platform | `apps/faf-ml-server` (:3100) + `apps/faf-ml-web` + `crates/faf-ml-core` | ✅ upload (drag&drop), triage badges, label view (edit boxes), dataset snapshots, datagen jobs (`POST /api/datagen` + polling + per-job/bulk sample deletion), Units page (fafcn-web unit browser ported: `/api/units` + portraits) |
| Icon↔unit mapping | `crates/faf-unit-tools` (`icon-map` subcommand) | ✅ 114 classes ↔ 501 units; artifact at `data/faf-ml/icon-map.json`; the Units page (`/units`) is its future UI home |
| SSD detector | `crates/faf-ml-model` (model/loss/anchors/data/predict + `train.rs` event-driven loop) | ✅ implemented, 26/26 tests; `apps/faf-ml-train` CLI is DELETED — training is a server capability |
| Training monitor | `/training` page + `GET /ws/training` + `GET /api/training/status` (`faf-ml-model::manager::TrainManager` actor driving `train()`; the server is a thin adapter) | ✅ REAL burn training in the server: live loss/valid charts (uPlot), pause/resume/stop/reset with instant ack (`pausing…`/`stopping…` settle at the batch boundary), server-side run registry (viewers attach/detach freely; `Attach` replays). mAP stays empty until the eval pass lands |
| MCP server | `apps/faf-ml-mcp` (rmcp, stdio) | ✅ 15 workflow-level tools (screenshots/datagen/datasets/training) — LLM agents drive the same API the web UI uses |

Not built yet: real mAP (predict+AP over the valid split), train-from-snapshot,
the eval/analysis view UI (Phase 3), Windows capture client.
See **Next tasks (priority order)** below for the full backlog.

**Design goal — one backend, two clients.** The platform exposes the same
typed JSON/WS API to everyone: humans walk the web UI's workflow (each page
shows its step banner), and LLM agents drive the identical steps through the
`faf-ml-mcp` MCP server (thin workflow-level tools; REST stays canonical).
Keep every future feature reachable from both paths.

## State at handover (evening 2026-09-10)

Today's session (all committed code, workspace compiles clean):

- **Platform UX finished**: CORS PATCH fix; label-view crash fixed (dioxus-web
  0.7.9 `WebImageEvent::as_any` bug — we read dims from metadata instead);
  datagen sliders + visual icon picker (`exclude_classes`); per-job/bulk
  synthetic deletion; kind-filtered snapshots + snapshot delete; Units browser
  + detail pages + Strategic-icons card (icon-map, nickname tooltips, shared
  icons, 4-faction filter); workflow Home + per-page step banners.
- **Real training in the backend** (the big refactor): loop moved into
  `crates/faf-ml-model/src/train.rs` (`TrainEvent` bus: Batch/EpochEnd/Done;
  `TrainControl` Continue/Pause/Abort; deterministic 10% valid split with a
  no-grad eval pass per epoch). Server: run registry + broadcast (`Attach`
  replays; disconnects never kill a run), busy guard, `GET
  /api/training/status`, `GET /api/runs`, `POST /api/predict(/annotate)`.
  MCP: `faf_ml_runs_list` + `faf_ml_predict` + status fallback to the
  registry. `apps/faf-ml-train` is deleted.

**Verified:** workspace compiles; registry plumbing live (batch-1 metrics
train=6.24 reached `GET /api/training/status` while running).
**NOT yet verified:** a full smoke run completing (epoch-end valid point,
checkpoint written, predict endpoints). It got blocked by speed, not code —
see below.

### First thing next session — finish the smoke verification

Run services in FOREGROUND terminals (user preference — no background tasks),
drive the smoke run through the platform itself (no ad-hoc scripts):

```bash
# terminal 1 (release server — debug NdArray conv is ~5 min/BATCH, unusable)
cargo run -p faf-ml-server --release
# terminal 2 (agent) or browser:
cargo xtask faf-ml mcp   # kimi with the faf-ml tools
```

Smoke run via MCP (or the Training page — same thing): `faf_ml_training_start`
with `epochs=1, max_batches=2, cpu=true` → watch `faf_ml_training_status`
until done → `faf_ml_runs_list` shows the checkpoint → `faf_ml_predict` on the
battle shot returns detections JSON. Browser equivalent: Training page Start
(epochs=1) → charts draw → status banner shows the run dir.

⚠ **Compute reality check**: this WSL box has NO real GPU — wgpu falls back
to llvmpipe (software). Real 50-epoch training should run on Windows (native
Vulkan) or a Linux box with GPU access. A full run here would take forever.

## Next tasks (priority order)

Ordered backlog. Work top-down; bump items only with a reason. When you finish
one, delete it here and update the tables/sections above to match.

### P0 — Verify the pipeline end-to-end

1. **Finish the smoke run** (`epochs=1, max_batches=2, cpu=true` via the
   Training page or `faf_ml_training_start`): epoch-end valid point appears,
   checkpoint lands in `data/faf-ml/runs/<ts>/`, `faf_ml_predict` returns
   detections on the battle shot. Then update "State at handover" above — it
   currently claims this is unverified, but a Sep-10 run dir exists; reconcile
   what actually happened.

### P1 — Make training trustworthy

2. **Seed the train/valid split** — `crates/faf-ml-model/src/train.rs:132`
   shuffles unseeded; the "deterministic 10% split" is only deterministic in
   count. Same seed → same split, or runs aren't comparable.
3. **Save the best checkpoint per epoch** (lowest valid loss), not just the
   final one — a diverging late epoch currently destroys the run's best state.
   Add early stopping and a simple LR schedule while you're in there.

### P2 — Data: close the domain gap

4. **Collect real screenshots** — only 4 background + 1 battle on disk vs. the
   ~20-across-several-maps target (step 1 below). One battle shot is not an
   exam.
5. **Calibrate `TEAM_COLORS`** in `crates/faf-ml-datagen/src/lib.rs` against
   real FAF player colors — player attribution depends on it (decision 3).
6. **Dataset compose view** (Phase 1 remainder).

### P3 — Evaluation

7. **Real mAP at epoch end** (predict + AP over the valid split) — valid loss
   alone can't tell you if detections are usable; the `map` field is stubbed
   `None` everywhere today.
8. **Phase 3 eval/analysis view** — detections → per-player unit table via
   `icon-map.json` + dominant-box-color attribution; run-registry UI in the
   monitor page; train-from-snapshot.

### P4 — Hygiene (cheap, do alongside anything above)

9. Sweep stale docs/comments: this doc says 193 classes but `classes.txt`
   has 210. (Done in the TrainManager refactor: `apps/faf-ml-server/README.md`
   header, `faf-ml-core/src/training.rs` / `faf-ml-model/src/lib.rs` doc
   comments referencing the deleted `faf-ml-train` CLI.)
10. Delete the dead YOLO-dir loader (`faf-ml-model/src/data.rs` `load_yolo_dir`)
    — nothing writes YOLO dirs since the datagen CLI was removed.
11. Add a faf-ml section to the root `STATUS.md` — the project is invisible
    at repo level.

### P5 — Scale up (only with a real GPU)

12. **Gradient accumulation** to lift the batch-4 wgpu buffer cap, and a
    prefetched/async data pipeline (PNG decode + anchor matching currently run
    synchronously on the training thread).
13. Move a real 50-epoch run to the RTX 3090 box; consider hard-negative
    mining / focal loss if background false positives dominate on real shots.

## ▶ Your next session, step by step

```bash
cd /home/zw/code/rust_programming/octopus

# 0. Start the platform
cargo xtask faf-ml backend          # → http://localhost:3100

# 1. COLLECT: in-game (Windows), take ~20 screenshots:
#    - EMPTY terrain (zoomed-out fog areas, several DIFFERENT maps) ← for backgrounds
#    - BUSY battles (several maps)                                  ← held-out exam
#    Upload ALL via the Gallery drop zone, then triage each card:
#    "background" = empty, "battle" = has units. (Default is "unclassified";
#    the "needs triage" filter shows what's left to mark.)

# 2. GENERATE synthetic data (reads ONLY background-marked shots):
#    open the Datagen view (http://localhost:3100/datagen), set count/size/
#    scale, Generate. Samples stream into the store while the job runs;
#    the jobs table polls until done.
#    Each sample is tagged with its job id, so a bad set is removed with the
#    job row's "Delete samples" button (deletes job + its samples). Older
#    untagged sets: Gallery → "synthetic" filter → "Clear N synthetic".
#    (Jobs live in server memory — a restart wipes the jobs table, but the
#    job→sample link is persisted in index.json.)

# 3. DOMAIN-GAP CHECK (5 min, do not skip): open a synthetic sample (Gallery
#    → "synthetic" filter) next to a REAL screenshot.
#    Icons must match the real render in SIZE + COLOR + sharpness.
#    If not: tune scale-min/scale-max and TEAM_COLORS in
#    crates/faf-ml-datagen/src/lib.rs, regenerate.

# 4. TRAIN on the platform store: open the Training page (or the MCP tools)
#    and Start a run (defaults: 50 epochs, batch 4, lr 1e-3, valid 10%).
#    ⚠ UNVERIFIED END-TO-END — see "State at handover" below; run the smoke
#    test first.
#    The run lives in the server registry — you can close the page and
#    re-attach later; checkpoints land in data/faf-ml/runs/<timestamp>/
#    (model.mpk + config.json). NOTE: run the RELEASE server for real
#    training (debug conv is painfully slow) and WSL has NO real GPU
#    (llvmpipe) — use cpu:false only with a real GPU, else cpu:true.

# 5. THE MOMENT OF TRUTH — predict on a HELD-OUT battle screenshot (all API):
curl -X POST localhost:3100/api/predict -H 'content-type: application/json' \
  -d '{"run":"<timestamp>","image_id":"<battle-shot-uuid>"}'
# annotated preview: same body → POST /api/predict/annotate > /tmp/pred.png
# open /tmp/pred.png: did it find the units?
```

### Reading the outcome of step 5

- **Works on real shots** → the synthetic approach is validated. Next: phase 2
  (training from the web UI) or the analysis view (detections → per-player
  table via `icon-map.json` + dominant-box-color for player attribution).
- **Works on synthetic, misses real** → domain gap. Iterate on datagen
  (colors/scale/AA), NOT on labeling by hand. Only if that stalls: mark some
  battle shots in the UI, let the model pre-label, correct, snapshot, mix into
  training.
- **Loss won't even go down on synthetic** → check anchor coverage /
  hyperparams; batch size is capped at 4 (see gotchas).

## Key decisions already made (don't re-litigate without reason)

1. **Classes = strategic icon names** (193 of them: units + markers +
   generics). Fine-grained on purpose — the analysis view aggregates via
   `faf-blueprints`. Markers/generics stay as classes so the model doesn't
   confuse them with units.
2. **Labeling UI is view+edit only** (no draw-from-scratch) — correction
   workflow, not a CVAT clone.
3. **Player attribution = dominant pixel color inside a detected box** — NOT
   an ML problem. But it means datagen's `TEAM_COLORS` must match real FAF
   player colors (currently approximate — calibrate from real screenshots).
4. **Learning workspace stays pure learning**; production model lives in
   `crates/faf-ml-model` (ch14 SSD concepts applied natively).
5. Icon-set orphans (`commander_*`, `experimental_*`, finer naval classes):
   the custom icon mod is finer than vanilla DB metadata; a curated alias
   table is a future task for the analysis view. 5 Nomads (`XNL*`) unit icons
   have no sprites — ignore unless you play with Nomads.

## Gotchas (hard-won, don't rediscover)

- **wgpu per-buffer cap ~128 MiB**: the detector's stem conv is stride-2 and
  default `--batch` is 4 because of this. Batch ≥6 panics with
  "can't allocate buffer". Fix = gradient accumulation (not implemented).
- **Training control commands** (pause/resume/stop/reset) ack instantly —
  the status flips to `pausing`/`stopping` immediately, and the settled
  `paused`/`stopped` arrives when the training thread reaches the next
  **batch boundary** (up to ~30 s on llvmpipe, ms on a real GPU). **Stop vs
  Reset**: both save the checkpoint (`data/faf-ml/runs/<ts>/`); Stop keeps
  the run record visible as a terminal state, Reset additionally wipes the
  run record and clears every viewer's charts, returning the manager to
  idle immediately (a new Start is accepted while the old thread finishes
  its last batch + save).
- **Train with `--release`.**
- Burn: `AdamConfig::with_grad_clipping` vs `SgdConfig::with_gradient_clipping`
  (inconsistent naming); BatchNorm/Dropout pick train/eval from
  `B::ad_enabled`, so eval needs `model.valid()` (the detector has no BN on
  purpose).
- Server on :3100; check for stale processes with `pgrep -f faf-ml-server`
  before assuming a new build is running ("Address already in use" bites).
- **No real GPU in WSL** (wgpu adapter = llvmpipe, software Vulkan): debug
  CPU conv takes minutes per batch. For real training use the RELEASE server;
  for actual speed, run where the RTX 3090 is visible.
- dx web builds go to `target/dx/faf-ml-web/{debug,release}/web/public`;
  the server serves the **release** build (`cargo xtask faf-ml build-web`).

## Reference material

- d2l.ai chapters that matter for this project: ch7 (convs), ch8 (esp. NiN's
  GAP, ResNet), ch14 (augmentation, anchors, SSD, NMS). Ports live in
  `~/code/rust_programming/dive_into_deep_learning_in_rust` with mdbook notes
  (`cargo xtask book` there).
- The icon↔blueprint reasoning: re-run
  `cargo run -p faf-unit-tools -- icon-map --out data/faf-ml/icon-map.json`.
- Platform READMEs: `apps/faf-ml-server/README.md`, `apps/faf-ml-web/README.md`.

## Phase roadmap (for orientation)

- **Phase 0 ✅** — platform: upload/triage/label/snapshot
- **Phase 1 ✅ (mostly)** — datagen is a server job driven from the web
  Datagen view; the `faf-datagen` CLI and the `/api/import/datagen` endpoint
  are gone (generation is internal, labels stream straight into the store).
  `faf_ml_model::train` reads the platform store directly.
  Remaining: dataset compose view
- **Next real milestone** — detector trained on synthetic data detecting
  units on a held-out real screenshot (steps 1–5 above)
- **Phase 2 ✅ (mostly)** — real burn training lives in the server:
  `faf-ml-model::train` (event-driven loop + validation split) driven by the
  `faf-ml-model::manager::TrainManager` actor (pause/resume/stop/reset with
  instant ack); `faf-ml-train` CLI deleted; predict is
  `POST /api/predict(/annotate)`. Remaining: real mAP at epoch end,
  train-from-snapshot, run registry UI in the monitor page
- **Phase 3** — eval/analysis view (per-player unit tables), correction loop,
  then the Windows capture client (eframe if GUI needed)
