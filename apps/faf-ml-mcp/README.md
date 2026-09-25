# faf-ml-mcp

MCP server (stdio) exposing the **faf-ml platform workflow as LLM tools** — a
thin wrapper over the `faf-ml-server` REST/WS API. One backend, two clients:
humans drive the web UI, agents drive these tools.

```
collect ──► triage ──► generate ──► review ──► snapshot ──► train
(upload)  (kind)     (datagen)   (labels)   (dataset)   (dummy for now)
```

## Tools (15)

| Tool | Purpose |
|---|---|
| `faf_ml_status` | orientation: API reachability, pool counts, dataset count — call first |
| `faf_ml_screenshots_list` | list screenshots, optional `kind` filter |
| `faf_ml_screenshots_upload` | multipart-upload local PNG paths (lands in `unclassified`) |
| `faf_ml_screenshot_triage` | mark a shot `battle` / `background` / `unclassified` |
| `faf_ml_screenshot_delete` | delete one screenshot (image + labels) |
| `faf_ml_datagen_start` | start a synthetic-data generation job (count/size/scale/seed; icon classes come from the server's icon config) |
| `faf_ml_datagen_jobs` | poll job progress |
| `faf_ml_datagen_job_delete` | delete a job AND its whole sample set |
| `faf_ml_synthetic_clear` | delete ALL synthetic samples (before regenerating a set) |
| `faf_ml_dataset_create` | immutable snapshot of chosen pools (default `synthetic`) |
| `faf_ml_datasets_list` | list snapshots |
| `faf_ml_dataset_delete` | delete a snapshot file |
| `faf_ml_training_start` | start a REAL training run (server-side, survives disconnects) → run handle |
| `faf_ml_runs_list` | list checkpoint runs (pick one for predict) |
| `faf_ml_predict` | run a checkpoint on a store screenshot → detections |
| `faf_ml_training_status` | progress + live losses/mAP (the MCP server owns the `/ws/training` socket internally) |
| `faf_ml_training_command` | pause / resume / stop / set_speed |

Design notes:

- **Workflow-level, not endpoint-level**: training's WebSocket is hidden
  behind `training_start`/`training_status`; uploads hide multipart framing.
- **REST stays canonical**: every tool is a thin call to
  `faf-ml-server` (`FAF_ML_API`, default `http://localhost:3100`). If this
  binary rots, nothing else breaks.
- Runs are in-memory: training handles and datagen-job visibility die with
  the processes that own them (samples/snapshots themselves are persisted
  server-side).

## Build & run

```sh
cargo build -p faf-ml-mcp            # → target/debug/faf-ml-mcp
FAF_ML_API=http://localhost:3100 ./target/debug/faf-ml-mcp   # speaks MCP on stdio
```

Logs go to stderr (stdout is the MCP channel).

## Register in an agent

**Easiest (Kimi CLI, session-scoped — global config untouched):**

```sh
cargo xtask faf-ml mcp   # builds faf-ml-mcp, then launches `kimi`
                         # with --mcp-config pointing at the binary
```

(Equivalent one-off: `kimi --mcp-config '{"mcpServers":{"faf-ml":{"command":"<abs path>/target/debug/faf-ml-mcp"}}}'`.)

**Permanent registration (Kimi CLI / Claude Code, `~/.<agent>/mcp.json`):**

```json
{
  "mcpServers": {
    "faf-ml": {
      "command": "/home/zw/code/rust_programming/octopus/target/debug/faf-ml-mcp",
      "env": { "FAF_ML_API": "http://localhost:3100" }
    }
  }
}
```

or `kimi mcp add --transport stdio faf-ml -- <abs path>/target/debug/faf-ml-mcp`.

Then, in a session: *"upload the PNGs in `~/shots`, mark the busy ones
battle and the rest background, generate 500 samples at scale 0.3–0.6,
snapshot as `v4`, start training, and tell me when it finishes."*

For a release binary use `cargo build -p faf-ml-mcp --release` and point the
config at `target/release/faf-ml-mcp`.
