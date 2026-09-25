# faf-ml-web

Dioxus 0.7 SPA for the **faf-ml** data platform (collect → review → generate
→ snapshot). Mirrors the `fafcn-web` structure: `main.rs` defines the routes,
`net.rs` resolves the API base URL (`http://localhost:3100` in debug builds,
same-origin in release), views live in `src/views/`, shared components in
`src/components/`. `src/workflow.rs` is the single source of truth for the
unit-detection workflow (ordered steps + routes): the Home page renders it as
a linked checklist and each step's page shows its `WorkflowBanner` prev/next
bar.

## Views

| Route | View | Purpose |
|---|---|---|
| `/` | `Home` | what this is + quick links |
| `/gallery` | `Gallery` | thumbnail grid, multi-file PNG upload (gloo-net + `web_sys::FormData` multipart POST), per-card triage (battle/background), delete, bulk "Clear N synthetic" on the synthetic filter |
| `/label/:id` | `Label` | image with SVG-rect overlay of the existing boxes (scaled via `viewBox` = natural size from `GET /api/screenshots` metadata); click a box to select, re-assign its class (dropdown from `GET /api/classes`), delete it, save (`PUT` labels). Review-only: no drawing new boxes. |
| `/units` | `Units` | unit database browser ported from fafcn-web (`src/components/unit_*` + `comparison_panel`): search, faction/kind/tech filters, multi-select compare panel; data from `GET /api/units[/meta]`, portraits from `GET /api/portraits/:id`, strategic overlays from `public/strategic/`; sidebar unit names link to the detail page |
| `/units/:id` | `UnitDetail` | one unit's portrait, identity badges, cost and non-zero economy-effect stats (`GET /api/units/:id`), plus a Strategic icons card (`GET /api/units/:id/icons`): blueprint default icon + custom-set classes mapped to this unit, with sharing units shown when an icon is ambiguous |
| `/icons` | `Icons` | icon configuration (workflow step 2): enable icon-set mods like in game (`GET /api/icons/sets` checkboxes), preview each unit's effective in-game icon on the unit grid with uncovered units highlighted (`GET /api/icons/units?mods=`), and pick the training icon classes (`GET /api/icons/classes`); persisted via `PUT /api/icons/config` |
| `/datagen` | `Datagen` | generation form (sliders → `POST /api/datagen`; icon classes come from the Icons page config; the 400 "mark backgrounds first" error is surfaced) + jobs table polling `GET /api/datagen/jobs` every 2 s while any job is running; Done rows link to the Gallery's synthetic filter and have a "Delete samples" button |
| `/training` | `Training` | live training monitor (currently a dummy pipeline): start/pause/resume/reset + speed control over `/ws/training` (fafcn simulate-page pattern), loss (train/cls/bbox/valid) and mAP charts via `faf-dioxus-ui::UplotChart` (uPlot assets in `public/`, wired in `Dioxus.toml`) |
| `/datasets` | `Datasets` | list immutable snapshots (name, #images, #boxes, date); create one from all current screenshots |

## Dev loop

```sh
# terminal 1 — API + (release) static hosting on :3100
cargo run -p faf-ml-server

# terminal 2 — hot-reload dev server (its own port, proxies nothing;
# the app calls localhost:3100 directly in debug builds)
cd apps/faf-ml-web && dx serve
```

## Build

```sh
cd apps/faf-ml-web && dx build --web --release
# output: target/dx/faf-ml-web/release/web/public
# (= the server's FAF_ML_WEB_DIST default; serve via faf-ml-server)
```

Tailwind v4 is compiled automatically by `dx` from `tailwind.css` at the app
root into `assets/tailwind.css` — no npm step.

## Explicitly NOT built yet

- training/eval UI, live metrics (phases 2–3)
- draw-new-box interactions, tagging/filtering, auth
