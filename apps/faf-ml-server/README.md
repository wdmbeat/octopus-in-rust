# faf-ml-server

Backend for the **faf-ml** data platform of the FAF unit-detection ML
project: collect screenshots → review bounding boxes → generate synthetic
training data → freeze immutable dataset snapshots → **train the SSD
detector** (real burn training with pause/resume/stop/reset over
`/ws/training`; the training manager actor lives in
`crates/faf-ml-model/src/manager.rs`, this server is a thin adapter).

Serves the `faf-ml-web` Dioxus build as static files (SPA fallback to
`index.html`) and exposes the JSON API below.

## Run

```sh
cargo run -p faf-ml-server
# env (defaults shown):
#   FAF_ML_PORT=3100
#   FAF_ML_DATA_DIR=data/faf-ml                                (gitignored)
#   FAF_ML_WEB_DIST=target/dx/faf-ml-web/release/web/public
#   FAF_ML_ICONS_DIR=tmp/custom-strategic-icons                (legacy flat sprite dir — fallback only)
#   FAF_ML_ICON_MODS=tmp/ReduxStrategicIconsLarge:tmp/Calibersexp:tmp/SACUIcons
#                                                              (colon-separated icon-set mod dirs)
```

Logs: stdout + `data/logs/faf-ml-server.log`.

## Endpoints

| Route | Purpose |
|---|---|
| `GET /api/health` | service status |
| `POST /api/screenshots` | multipart PNG upload (one or more `files` fields) → stores + returns `ScreenshotMeta[]` |
| `GET /api/screenshots` | list all `ScreenshotMeta` (from `index.json`) |
| `GET /api/screenshots/{id}/image` | serve the PNG |
| `GET /api/screenshots/{id}/labels` | box list (JSON, `[]` when unlabeled) |
| `PUT /api/screenshots/{id}/labels` | replace the box list |
| `DELETE /api/screenshots/{id}` | remove image + labels + index entry |
| `DELETE /api/screenshots?kind={kind}` | bulk-remove every screenshot of one kind (image + labels + index entries); `kind` is mandatory. Clearing `synthetic` also drops finished datagen jobs from the registry |
| `GET /api/classes` | class names from `classes.txt` |
| `POST /api/datagen` | body = `DatagenConfig` → start a generation job: composites sprites onto `background`-kind screenshots (400 when none exist — triage in the Gallery first), streams each sample into the store as a `synthetic` screenshot + labels JSON, merges sprite class names into `classes.txt`. The sprite pool comes from `icon-config.json` (enabled icon-set mods minus excluded classes — see `/api/icons/*`); `classes.txt` still merges ALL enabled sprite classes so class ids stay stable |
| `GET /api/datagen/sprites` | sorted class names of every sprite in the legacy flat icons dir (superseded by `/api/icons/classes`) |
| `GET /api/datagen/sprites/{class}/image` | the sprite as PNG (the source DDS is not browser-displayable) |
| `GET /api/datagen/jobs` | all datagen jobs (newest first) |
| `GET /api/datagen/jobs/{id}` | one job (the web UI polls this while `running`) |
| `DELETE /api/datagen/jobs/{id}` | remove a finished job AND its generated sample set (400 while `running`); samples predate job tracking → use bulk delete instead |
| `GET /api/datasets` | list dataset manifests |
| `DELETE /api/datasets/{name}` | remove a snapshot file (404 when unknown; "immutable" = never mutated, not undeletable) |
| `POST /api/datasets` | `{name, image_ids}` or `{name, kinds}` → immutable snapshot embedding the current labels (409 if the name exists, 400 when nothing selected). `kinds` resolves ids from the screenshot index by kind (e.g. `["synthetic"]` for a training set) and takes precedence |
| `GET /api/units` | all unit summaries (4 playable factions) from the shared ETFreeman unit database (`faf-blueprints`; override the units file with `FAFCN_UNITS_FILE`) |
| `GET /api/units/meta` | unit database version + upstream attribution |
| `GET /api/units/{id}` | one unit summary (case-insensitive exact id; 404 when unknown) |
| `GET /api/units/{id}/icons` | the unit's blueprint default icon + every custom-set icon class mapped to it from `icon-map.json`; sharing units are `{id, name}` pairs, four FAF factions only (mod factions like Nomads excluded) |
| `GET /api/icons/sets` | registered icon-set mods (`FAF_ML_ICON_MODS`) with class/assignment counts and enabled flags (parsed from `mod_info.lua` + `mod_icons.lua`) |
| `GET/PUT /api/icons/config` | read/persist `icon-config.json` (`enabled_mods`, `excluded_classes`; absent file = all mods enabled). Unknown mod ids → 400 |
| `GET /api/icons/classes[?mods=a,b]` | per-class source set, unit coverage and excluded flag for the picker; only unit-mapped classes are listed (orphan marker icons like `strat_attack`/`ferry_point` are dropped) |
| `GET /api/icons/units[?mods=a,b]` | effective strategic icon per unit under the selection (explicit mod assignment > blueprint default > uncovered), four factions only |
| `GET /api/icons/sprites/{class}/image` | the class's `_rest` sprite as PNG, resolved across enabled sets (a mod's sprite overrides the base set's, like in game) |
| `GET /api/portraits/{id}` | unit portrait PNG from `FAF_ML_PORTRAITS_DIR` (default `assets/icons/units`) |
| `GET /ws/training` | WebSocket training (fafcn `/ws/simulate` pattern): `Start {config, speed}` starts a REAL burn training run (the `faf-ml-model` training manager actor spawns a dedicated thread); `Attach` replays + streams the current/last run without starting anything. The run lives server-side and survives viewer disconnects; `Command` frames pause/resume/stop/reset/set-speed. Commands ack instantly (`pausing`/`stopping` status) and take effect at the next batch boundary; stop and reset both save the checkpoint — reset also wipes the run record and tells viewers to clear charts |
| `GET /api/training/status` | the training registry as JSON (config, status, points, latest; terminal statuses carry `run_dir`) — 404 before the first run |
| `GET /api/runs` | checkpoint runs under `runs/` (name = timestamp, class count) |
| `POST /api/predict` | `{run, image_id, score_threshold?, cpu?}` → detections JSON (class, score, pixel box) |
| `POST /api/predict/annotate` | same body → annotated PNG bytes |

Generation logic lives in `crates/faf-ml-datagen` (the former `faf-datagen`
CLI, now a library); shared wire types (`DatagenConfig`, `DatagenJob`,
`DatagenStatus`, …) live in `crates/faf-ml-core`.

## Data layout

```
data/faf-ml/
  classes.txt              one class per line (line no. = class id)
  screenshots/<uuid>.png   uploaded images
  screenshots/index.json   [ScreenshotMeta]
  labels/<uuid>.json       [LabeledBox]  (absolute pixels)
  datasets/<name>.json     DatasetManifest (labels embedded → immutable)
```

## Explicitly NOT built yet

- eval view (phase 3)
- draw-new-box interactions (review/edit only), tagging/filtering, auth
- Windows capture client (later — eframe if a GUI is needed)

## Dev loop

Terminal 1: `cargo run -p faf-ml-server`
Terminal 2: `cd apps/faf-ml-web && dx serve` (hot-reload UI on its own port,
talks to `localhost:3100` in debug builds — see `src/net.rs`).

Quick smoke test:

```sh
curl localhost:3100/api/health
curl -X POST localhost:3100/api/screenshots -F "files=@/path/to/shot.png"
# after marking a shot as background:
curl -X POST localhost:3100/api/datagen \
  -H 'Content-Type: application/json' -d '{"count":10}'
curl localhost:3100/api/datagen/jobs
```
