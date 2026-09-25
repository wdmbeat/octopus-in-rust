# How to Deploy / Update fafcn on the `majiko` Server (LLM Runbook)

This document is written to be **executed by an LLM agent** (Kimi Code CLI,
Claude Code, etc.). It records the exact procedure that was used to deploy
fafcn to the `majiko` home server on 2026-08-19, including the pitfalls that
were hit and how they were fixed. Updated 2026-08-28: the site moved from
`https://8v.pub:10041` to **`https://faforever.cn:60`** — a domain registered
on AliCloud (see the "Public domain" section). For the generic runbook (fresh
server, nginx + TLS, build-on-server option) see `how_to_deploy_fafcn.md` in
the same directory. For a condensed ops summary (credentials index, renewal reminders,
troubleshooting tree) see `majiko-maintenance-note.md` in the same directory.

> **Sensitive values:** all secrets below are shown as `<PLACEHOLDERS>`.
> Real values live only on the machines themselves:
>
> - SSH/sudo password — ask the user; never write it into files or git.
> - LLM API key + upload token — already present in `/opt/fafcn/.env` on the
>   server (git-ignored). Reuse them; do not regenerate unless the user asks.
> - **Note:** the server `.env` no longer matches the local
>   `apps/fafcn-server/.env` — the server was switched to a custom LLM
>   provider (see the "LLM provider" section). Do NOT blindly re-copy the
>   local `.env` over the server one; if you must (Phase 4), re-apply the
>   three `FAFCN_LLM_*` lines afterwards.

## Server facts (verified 2026-08-19; public URL updated 2026-08-28)

| Fact | Value |
|---|---|
| SSH | `majiko@8v.pub -p 10040` (password auth; sudo uses the same password) |
| LAN IP | `192.168.50.10` (behind a firewall/NAT; admin forwards ports on request) |
| Public access | external **TCP 60** → `192.168.50.10:3000` → `https://faforever.cn:60` (since 2026-08-28; previously TCP 10041 → `https://8v.pub:10041`) |
| External 80/443 | **NOT available** on the server itself — HTTPS is terminated at the friend's gateway reverse proxy (see "Firewall / HTTPS note") |
| Internet from server | GitHub etc. **blocked**; proxy gateway available at `http://192.168.50.1:7893` if ever needed (`export https_proxy=http://192.168.50.1:7893`) |
| OS / resources | Ubuntu 22.04, 4 cores, 5.8 GB RAM, ~34 GB free disk |
| Toolchain on server | none (no git/rust/nginx) and **not needed** — everything is built locally and shipped |
| Install dir | `/opt/fafcn` (owned by `majiko`) |
| Service | systemd unit `fafcn.service`, `EnvironmentFile=/opt/fafcn/.env` |

Because GitHub is unreachable from the server and no toolchain exists there,
**always build locally and rsync artifacts** (Option B of the generic
runbook). Do not attempt `git clone` on the server.

## Network configuration (the complete picture)

**One single application port carries EVERYTHING.** fafcn-server multiplexes
the web SPA, JSON APIs, the simulator WebSocket (`/ws/simulate`), and all
gamedata mirror traffic — player downloads AND token-authenticated uploads
(`fafcn-sync upload`) — over plain HTTP on TCP 3000. There are no separate
ports, no raw TCP/UDP services, nothing else to forward. **Never request
additional port forwards for sync/upload features.**

### Inbound (the only two forwarded ports)

| External | Internal | Protocol | Purpose |
|---|---|---|---|
| TCP 10040 | `192.168.50.10:22` | SSH | server administration (password auth) |
| TCP 60 | `192.168.50.10:3000` | TCP / HTTP | **all fafcn traffic**: web, API, WebSocket, gamedata up/download (since 2026-08-28; previously TCP 10041 via `8v.pub`) |

External 80/443 are **blocked by the residential ISP** — that is why the
public URL carries a port (`:60`, previously `:10041`). Server-side `ufw` is
**inactive** — the
edge firewall is the only packet filter; keep it that way (do not "harden"
ufw on the server without adding an SSH allow rule first, or you will lock
everyone out).

### Outbound (from the server)

| Destination | Reachability |
|---|---|
| LLM relay `llmapi.secsino.com` | ✅ direct (domestic service, no proxy) |
| Ubuntu apt mirrors | ✅ direct |
| GitHub / most non-CN sites | ❌ blocked — use `export https_proxy=http://192.168.50.1:7893` if ever needed |

### Traffic flow

```
player browser / fafcn-sync (download)          uploader's fafcn-sync (upload)
        │ https://faforever.cn:60                      │  + Bearer UPLOAD_TOKEN
        └──────────────┬─────────────────────────────────┘
                       ▼
      friend's Lucky reverse proxy (terminates HTTPS, holds the
      faforever.cn cert; also runs DDNS + ACME for the domain)
                       ▼  plain HTTP, external TCP 60 (edge forward)
              192.168.50.10:3000  fafcn-server (plain HTTP, single port)
                       │
                       ├── web SPA / JSON APIs
                       ├── /ws/simulate (WebSocket upgrade, same port)
                       └── /api/gamedata/... (mirror: manifest, files, upload)
```

### If connectivity breaks, check in this order

1. `curl -s http://127.0.0.1:3000/api/health` **on the server** — if this
   fails, the service is down (`journalctl -u fafcn -n 50`), network is fine.
2. `curl -s https://faforever.cn:60/api/health` from outside — if this fails
   but step 1 works, the **edge forward** is broken (most common cause: the
   machine's LAN IP changed via DHCP — ask the admin to update the rule or
   pin the IP). If DNS no longer resolves to the friend's current public IP,
   his Lucky DDNS task is down — ask him to check it.
3. If only uploads fail with 401/403: wrong/rotated `UPLOAD_TOKEN`, not a
   network issue.

## Layout on the server

```
/opt/fafcn/
├── bin/fafcn-server                      # release binary (built locally)
├── web-dist/                             # dx release bundle (target/dx/.../public)
├── assets/icons/units/                   # unit portraits
├── config/faf_units.json                 # from plugins/faf-units/data/
├── data/
│   ├── qqbot-data/plugins/faf_units_plugin.wasm
│   ├── faf-gamedata/                     # mirror: channels/ files/ incoming/ client/ (~800 MB)
│   └── logs/fafcn-server.log
└── .env                                  # secrets + path overrides (chmod 600)
```

`/opt/fafcn/.env` contains the local `apps/fafcn-server/.env` values plus
these overrides:

```bash
FAFCN_PORT=3000
FAFCN_WEB_DIST=/opt/fafcn/web-dist
FAFCN_PORTRAITS_DIR=/opt/fafcn/assets/icons/units
FAFCN_PLUGINS_DIR=/opt/fafcn/data/qqbot-data/plugins
FAFCN_GAMEDATA_DIR=/opt/fafcn/data/faf-gamedata
FAFCN_GAMEDATA_CLIENT_DIR=/opt/fafcn/data/faf-gamedata/client
FAFCN_UNITS_FILE=/opt/fafcn/config/faf_units.json
```

## LLM provider (Q&A page) — custom relay, NOT the default Kimi

Since 2026-08-19 the Q&A page on majiko uses a **third-party OpenAI-compatible
relay** (new-api style), not the Kimi key from the local dev `.env`:

```bash
# /opt/fafcn/.env — current live values (key redacted here)
FAFCN_LLM_PROVIDER_TYPE=openai_compatible
FAFCN_LLM_BASE_URL=https://llmapi.secsino.com/v1
FAFCN_LLM_MODEL=deepseek-v4-flash        # alternative: deepseek-v4-pro
FAFCN_LLM_API_KEY=<SECSINO_API_KEY>      # ask the user; belongs to a friend's account
```

Facts and notices (all learned the hard way on 2026-08-19):

- **This is NOT official DeepSeek.** Official `api.deepseek.com` rejects this
  key (`Authentication Fails ... invalid`). Only ever point
  `FAFCN_LLM_BASE_URL` at `https://llmapi.secsino.com/v1` for this key.
- The relay **hides its model list** (`GET /v1/models` returns an empty
  `data` array). Don't probe it to discover models; ask the user/friend for
  the exact model name. Confirmed working: `deepseek-v4-flash`,
  `deepseek-v4-pro`.
- Error `No available channel for model <X> under group vip (distributor)`
  means the **key authenticates** but its group has no channel for that
  model (wrong model name, zero balance, or group not provisioned). It is an
  account/platform problem — no client-side change will fix it; send the
  `request id` from the error body to the platform owner.
- Both models are **reasoning models**: responses spend tokens on
  `reasoning_content` first. With tiny `max_tokens` the visible `content`
  can come back empty with `finish_reason: "length"` — this is expected, not
  a bug. First-token latency is slightly higher than the old Kimi setup.
- The server reaches the relay **directly, no proxy needed** (domestic
  service).
- **Cost warning:** Q&A is public; every visitor's question bills the
  friend's key. Mention this to the user if they seem unaware.
- **Backup/rollback:** the pre-switch config (Kimi `kimi-for-coding`) is
  saved at `/opt/fafcn/.env.bak-kimi` on the server. Roll back with:

  ```bash
  $SSH 'cp /opt/fafcn/.env.bak-kimi /opt/fafcn/.env && echo "<SSH_PASSWORD>" | sudo -S -k systemctl restart fafcn 2>/dev/null'
  ```

- **Switching provider/model** is a 3-line `sed` on `/opt/fafcn/.env`
  (`FAFCN_LLM_BASE_URL`, `FAFCN_LLM_MODEL`, `FAFCN_LLM_API_KEY`) plus
  `sudo systemctl restart fafcn`. **Always verify a new provider/key from
  the server with curl first**, then gate on the health check:

  ```bash
  # 1. direct provider test (expect a chat.completion JSON, not an error object)
  $SSH 'curl -s --max-time 30 <BASE_URL>/chat/completions \
    -H "Content-Type: application/json" -H "Authorization: Bearer <KEY>" \
    -d "{\"model\":\"<MODEL>\",\"messages\":[{\"role\":\"user\",\"content\":\"say pong\"}],\"max_tokens\":64}"'

  # 2. after .env edit + restart, end-to-end gate (expect "status":"ok")
  curl -s --max-time 60 https://faforever.cn:60/api/health
  ```

  `GET /api/health` is the composite gate: standard service health
  (units loaded, web dist, portraits, gamedata dirs) PLUS a REAL LLM
  round-trip in one call. `"status":"ok"` proves deployment, key validity,
  reachability, and model availability in one shot. Per-component detail is
  in `service.*` and `qa.*`; a broken LLM alone yields HTTP 200 with
  `"status":"degraded"` + `qa.status="error"` (the rest of the site still
  works). `GET /api/health/qa` remains as the LLM-only variant (503 on
  provider failure). Neither endpoint should be polled aggressively — each
  call spends a few LLM tokens.

## Shell conventions (IMPORTANT — read before running anything)

1. **Single-quote the password.** It contains `!`, which triggers bash
   history expansion inside double quotes (`event not found`):

   ```bash
   SSH="sshpass -p '<SSH_PASSWORD>' ssh -p 10040 -o StrictHostKeyChecking=no majiko@8v.pub"
   RSYNC_SSH="sshpass -p '<SSH_PASSWORD>' ssh -p 10040 -o StrictHostKeyChecking=no"
   ```

2. **sudo on the server** needs the same password piped via stdin:

   ```bash
   $SSH 'echo "<SSH_PASSWORD>" | sudo -S -k <command> 2>/dev/null'
   ```

3. Every remote verification should go through the **public URL**
   `https://faforever.cn:60` as well as `127.0.0.1:3000` on the server — the
   port-forward rule can break independently of the service.

## Full redeploy from scratch (if /opt/fafcn was wiped)

Run from the local repo root (`~/code/rust_programming/octopus`).

### Phase 1 — Build locally

```bash
cargo build --release -p fafcn-server
cargo build --release -p faf-units-plugin --target wasm32-unknown-unknown
(cd apps/fafcn-web && rm -rf ../../target/dx/fafcn-web/release && dx build --release --platform web)
```

**Gate 1a (web bundle is a TRUE release build — see Pitfall 2):**

```bash
ls target/dx/fafcn-web/release/web/public/assets/ | wc -l        # expect ~5, not dozens
grep -c "localhost:3000" target/dx/fafcn-web/release/web/public/assets/fafcn-web_bg-*.wasm
# expect EXACTLY 1 (the unreachable fallback in api_base()); 2+ means a debug build — DO NOT SHIP
```

### Phase 2 — Provision directories

```bash
$SSH 'echo "<SSH_PASSWORD>" | sudo -S -k bash -c "
  mkdir -p /opt/fafcn/{bin,data/qqbot-data/plugins,data/faf-gamedata,data/logs,web-dist,assets/icons,config} &&
  chown -R majiko:majiko /opt/fafcn" 2>/dev/null'
```

### Phase 3 — Ship artifacts

```bash
rsync -az -e "$RSYNC_SSH" target/release/fafcn-server                       majiko@8v.pub:/opt/fafcn/bin/
rsync -az -e "$RSYNC_SSH" target/wasm32-unknown-unknown/release/faf_units_plugin.wasm majiko@8v.pub:/opt/fafcn/data/qqbot-data/plugins/
rsync -az --delete -e "$RSYNC_SSH" target/dx/fafcn-web/release/web/public/  majiko@8v.pub:/opt/fafcn/web-dist/
rsync -az -e "$RSYNC_SSH" assets/icons/units                                majiko@8v.pub:/opt/fafcn/assets/icons/
rsync -az -e "$RSYNC_SSH" plugins/faf-units/data/faf_units.json             majiko@8v.pub:/opt/fafcn/config/
rsync -az --partial -e "$RSYNC_SSH" data/faf-gamedata/                      majiko@8v.pub:/opt/fafcn/data/faf-gamedata/
```

Notes:

- `--delete` on `web-dist` is **mandatory** (hashed asset filenames
  accumulate; stale debug bundles must not survive — see Pitfall 2).
- `--delete` must **NOT** be used on `data/faf-gamedata/` — uploaded mirror
  content may exist only on the server.
- The gamedata rsync is ~800 MB; expect ~1 min at ~14 MB/s. Use
  `run_in_background` if the agent supports it.

### Phase 4 — `.env` (only if missing on the server)

Reuse the local secrets, then append the path overrides.

> **WARNING:** the local `apps/fafcn-server/.env` still contains the old Kimi
> LLM key. After copying it, you MUST re-apply the custom LLM provider lines
> from the "LLM provider" section above (`FAFCN_LLM_BASE_URL`,
> `FAFCN_LLM_MODEL`, `FAFCN_LLM_API_KEY`), then verify with the health gate.
> If `/opt/fafcn/.env` already exists on the server, SKIP this phase entirely.

```bash
rsync -az -e "$RSYNC_SSH" apps/fafcn-server/.env majiko@8v.pub:/opt/fafcn/.env
$SSH 'cd /opt/fafcn &&
  sed -i -e "s|^FAFCN_PLUGINS_DIR=|#&|" -e "s|^FAFCN_PORT=|#&|" .env &&
  cat >> .env << "EOF"

FAFCN_PORT=3000
FAFCN_WEB_DIST=/opt/fafcn/web-dist
FAFCN_PORTRAITS_DIR=/opt/fafcn/assets/icons/units
FAFCN_PLUGINS_DIR=/opt/fafcn/data/qqbot-data/plugins
FAFCN_GAMEDATA_DIR=/opt/fafcn/data/faf-gamedata
FAFCN_GAMEDATA_CLIENT_DIR=/opt/fafcn/data/faf-gamedata/client
FAFCN_UNITS_FILE=/opt/fafcn/config/faf_units.json
EOF
chmod 600 .env'
```

### Phase 5 — systemd (only if the unit is missing)

```bash
$SSH 'echo "<SSH_PASSWORD>" | sudo -S -k bash -c "cat > /etc/systemd/system/fafcn.service << EOF
[Unit]
Description=fafcn-server (FAF CN web + gamedata mirror)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=majiko
Group=majiko
WorkingDirectory=/opt/fafcn
EnvironmentFile=/opt/fafcn/.env
ExecStart=/opt/fafcn/bin/fafcn-server
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload && systemctl enable --now fafcn" 2>/dev/null'
```

### Phase 6 — Verification gates

```bash
$SSH 'systemctl is-active fafcn && curl -s http://127.0.0.1:3000/api/health'
```

- Gate 1: `active` and health JSON contains `"status":"ok"`.
  If the service crash-loops, read
  `journalctl -u fafcn -n 50 --no-pager` (see Pitfall 1).

```bash
curl -s https://faforever.cn:60/api/health             # via public URL
curl -s https://faforever.cn:60/api/gamedata/status    # expect channels: gamedata, map-generator, faf-client
curl -s -o /dev/null -w "%{http_code}\n" https://faforever.cn:60/
```

- Gate 2: all return 200 / expected JSON through the **public** address.
- Gate 3 (browser): ask the user to hard-refresh `https://faforever.cn:60/`
  (Ctrl+Shift+R) and confirm the home page renders with units loaded —
  this catches a debug web bundle that API checks cannot.

## Routine update (code changed, redeploy)

**One command** (implemented in `xtask/src/apps/fafcn_majiko.rs`):

```bash
cargo xtask fafcn majiko-deploy                   # full stack: backend + plugin + web
cargo xtask fafcn majiko-deploy --skip-web        # backend/plugin only, keep the web UI
cargo xtask fafcn majiko-deploy --with-gamedata   # also sync the ~800MB mirror (rarely needed)
cargo xtask fafcn majiko-deploy --skip-verify     # skip health gates (e.g. edge forward known-down)
```

When ONLY the fafcn-sync Windows client changed, skip the full redeploy —
this rebuilds the client (release, fresh build tag), rsyncs just
`data/faf-gamedata/client/` (exe + VERSION), and verifies the new tag shows
up in `/api/gamedata/status`. No service restart needed (the server reads
those files per request):

```bash
cargo xtask fafcn majiko-deploy-file-sync
```

Companion command — three-layer health report (SSH login → systemd +
`127.0.0.1:3000` on the host → public `MAJIKO_PUBLIC_URL`); exits non-zero
if any layer fails, so it doubles as the connectivity decision tree from
"Network configuration":

```bash
cargo xtask fafcn majiko-health
```

It performs, in order: preflight (local `sshpass`/`rsync`/`dx` + SSH login
check) → release builds (server, wasm plugin, dx web with a cleaned output
dir) → the Pitfall-2 release-bundle gate → rsync (with `--delete` on
web-dist only) → `systemctl restart fafcn` → health gates on both
`127.0.0.1:3000` and `MAJIKO_PUBLIC_URL`.

Connection settings and the SSH password live in **`xtask/.env`**
(git-ignored; template: `xtask/.env.example`). Keys: `MAJIKO_SSH_PASSWORD`
(required), `MAJIKO_SSH_USER`, `MAJIKO_SSH_HOST`, `MAJIKO_SSH_PORT`,
`MAJIKO_DEPLOY_DIR`, `MAJIKO_PUBLIC_URL` (update this one if the friend's
TLS reverse proxy changes the public address). Process env overrides
`xtask/.env` values.

Mirror data, `.env`, and the systemd unit survive updates untouched.

<details>
<summary>Manual equivalent (what the command does, step by step)</summary>

```bash
cargo build --release -p fafcn-server
(cd apps/fafcn-web && dx build --release --platform web)   # only if web changed
rsync -az -e "$RSYNC_SSH" target/release/fafcn-server majiko@8v.pub:/opt/fafcn/bin/
rsync -az --delete -e "$RSYNC_SSH" target/dx/fafcn-web/release/web/public/ majiko@8v.pub:/opt/fafcn/web-dist/
$SSH 'echo "<SSH_PASSWORD>" | sudo -S -k systemctl restart fafcn 2>/dev/null; sleep 4; systemctl is-active fafcn'
```

Then re-run the Phase 6 gates.
</details>

## Pitfalls hit on 2026-08-19 (fixed, but know them)

### Pitfall 1 — `workspace_root()` panic on the server

Symptom: service crash-loops;
`journalctl -u fafcn` shows
`panicked at apps/fafcn-server/src/config.rs: failed to resolve workspace root`.

Cause: the workspace root was derived from compile-time
`CARGO_MANIFEST_DIR` + `.canonicalize().expect(...)`; the compile-time path
does not exist on the deploy host.

Fix (already in source — do not revert):
`apps/fafcn-server/src/config.rs` `workspace_root()` now falls back to the
unresolved path when `canonicalize()` fails. All real paths on the server
come from `FAFCN_*` env overrides, so the fallback is never dereferenced.

### Pitfall 2 — shipping a DEBUG web bundle

Symptom: page loads but sections are missing; home page shows
`Failed to load units: TypeError: Failed to fetch`. API curls return 200.

Cause: `apps/fafcn-web/src/net.rs` `api_base()` hardcodes
`http://localhost:3000` when `cfg!(debug_assertions)` is on, so the
browser calls the visitor's own localhost. The stale `target/dx` output dir
had accumulated many hashed bundles and the active one was a debug build.

Prevention (already encoded in the gates above):

- Always `rm -rf target/dx/fafcn-web/release` before `dx build --release --platform web`.
- Always `rsync --delete` the web-dist.
- Gate 1a: the shipped `fafcn-web_bg-*.wasm` must contain **exactly 1**
  occurrence of `localhost:3000` (the release-mode `unwrap_or_else`
  fallback). More = debug build.
- After deploy, tell the user to hard-refresh (Ctrl+Shift+R) — browsers
  cache the old JS/wasm aggressively.

## Public domain (`faforever.cn`, registered on AliCloud)

Since 2026-08-28 the site is served as **`https://faforever.cn:60`**. The
domain is registered on **AliCloud** (registrant: the user, personal;
registered 2026-08-25, expires 2027-08-25; real-name verification done).
Renewal is a normal AliCloud console operation — set a reminder for
~2027-07, an expired domain takes the whole site down.

The split of responsibilities:

- **AliCloud DNS** (we manage, `aliyun` CLI profile `default` works):
  hosts the zone. The only record needed is an **A record `faforever.cn` →
  the friend's current public IP** (113.5.92.224 at migration time).
- **The friend's Lucky gateway** (he manages): runs a **DDNS task** that
  keeps the A record in sync with his dynamic public IP via the AliCloud
  DNS API, requests/renews the TLS cert (ACME **DNS-01**, Let's Encrypt),
  and reverse-proxies `faforever.cn:60` → `192.168.50.10:3000`.
- For that API access the friend holds a **dedicated AliCloud RAM
  sub-account AccessKey** scoped to DNS operations on `faforever.cn` only
  (custom policy on `acs:alidns:*:*:domain/faforever.cn`). It cannot touch
  ECS or other domains. If it ever leaks, revoke it in the RAM console and
  issue a new one the same way. Never hand out the main-account or the
  local CLI's AccessKey.
- **ICP filing (备案):** a `.cn` domain pointed at a mainland-China
  residential line normally requires ICP registration for web service on
  80/443. Those ports are ISP-blocked here anyway; running on a
  non-standard port (`:60`) is the current working state, but be aware the
  site could theoretically be asked to file. If that ever happens, the
  fallback is going back to `8v.pub` or moving the frontend to a filed
  host.

The old address `https://8v.pub:10041` may stay up as a legacy alias — do
not rely on it; all docs, clients, and `MAJIKO_PUBLIC_URL` use the new
domain.

## Firewall / HTTPS note

Port forwarding and connectivity troubleshooting live in "Network
configuration" above. This section covers only HTTPS.

### HTTPS: TLS termination at the gateway (Lucky)

The site is served as **`https://faforever.cn:60`**. TLS is **not** done on
this server — the network admin (the user's friend) terminates it on his
own reverse proxy (Lucky) in front of the port forward. The certificate is
a real **Let's Encrypt** cert for `faforever.cn`, issued via ACME DNS-01
against AliCloud DNS and auto-renewed by Lucky (before Lucky was
configured, the port served Lucky's self-signed default cert — if you ever
see `CN=Lucky` again on the public port, the domain vhost/cert binding is
broken on his side):

```
browser ──https──▶ friend's Lucky reverse proxy (holds the faforever.cn cert)
                   ──http──▶ 192.168.50.10:3000 (fafcn-server, plain HTTP)
```

Consequences for anyone maintaining this deployment:

- **Server side stays plain HTTP on port 3000. Do NOT add TLS, nginx, or
  certbot here.** Nothing to install, nothing to renew on this machine.
- The friend's reverse proxy forwards these headers (required, or features
  break):

  ```nginx
  proxy_set_header Host $host;                      # required
  proxy_set_header X-Forwarded-Proto $scheme;       # required — see below
  proxy_set_header Upgrade $http_upgrade;           # required for /ws/simulate
  proxy_set_header Connection "upgrade";            # required for /ws/simulate
  proxy_read_timeout 3600s;                         # Q&A streaming is long-lived
  ```

- **Why `X-Forwarded-Proto` matters:** fafcn-server embeds
  `<scheme>://<host>` into every sync-client exe it serves (the /sync page
  download). If the header is missing, players get a client pre-filled with
  an `http://` mirror address while the site is actually served over HTTPS.
- If the public URL ever changes, update `MAJIKO_PUBLIC_URL` in `xtask/.env`
  (or the default in `xtask/src/apps/fafcn_majiko.rs`) and the references in
  this document, then re-verify:

  ```bash
  cargo xtask fafcn majiko-health    # all three layers should go green
  # plus a browser check of /qa and the WebSocket simulator page
  ```

- The upload token now travels inside TLS (`fafcn-sync upload --server
  https://faforever.cn:60 ...`). Rotate it by editing `/opt/fafcn/.env` +
  `systemctl restart fafcn` if ever concerned.

## Ops cheat sheet

```bash
# logs
$SSH 'echo "<SSH_PASSWORD>" | sudo -S -k journalctl -u fafcn -f 2>/dev/null'
$SSH 'tail -f /opt/fafcn/data/logs/fafcn-server.log'

# restart / status
$SSH 'echo "<SSH_PASSWORD>" | sudo -S -k systemctl restart fafcn 2>/dev/null'
$SSH 'systemctl is-active fafcn'

# upload gamedata from a FAF machine (token is in /opt/fafcn/.env)
fafcn-sync upload --server https://faforever.cn:60 --token <UPLOAD_TOKEN> --dir "C:\ProgramData\FAForever"
```
