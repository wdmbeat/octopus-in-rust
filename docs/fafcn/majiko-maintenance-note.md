# FAFCN Maintenance Note (updated 2026-08-31)

> Secrets are referenced by **location only**, never written here in
> plaintext. The real values live only in the files/consoles listed below.

## 1. Site basics

| Item | Value |
|---|---|
| Public URL | **https://faforever.cn:60** (legacy alias `https://8v.pub:10041` — do not rely on it) |
| Domain | `faforever.cn`, registered on **AliCloud** (personal, real-name verified), registered 2026-08-25, **expires 2027-08-25** |
| ⚠️ Renewal reminder | **Renew in the AliCloud console before ~July 2027** — expiry takes the whole site down |
| Why the port | The friend's residential ISP blocks 80/443, so a non-standard port (`:60`) is required |
| ICP filing | Not filed; a `.cn` domain on a mainland IP could theoretically be asked to file. Current non-standard port works. Fallback if forced: return to `8v.pub` or move to a filed host |

## 2. Architecture & responsibilities

```
player browser / fafcn-sync
        │ https://faforever.cn:60
        ▼
friend's Lucky gateway (113.5.92.224, dynamic IP)
  · DDNS: keeps the A record in sync via AliCloud DNS API
  · TLS cert: Let's Encrypt via ACME DNS-01, auto-renewed
  · reverse proxy: faforever.cn:60 → 192.168.50.10:3000
  · port forwards: 10040→22 (SSH), 60→3000 (site)
        ▼  plain HTTP
majiko home server 192.168.50.10 (Ubuntu 22.04)
  · fafcn-server carries EVERYTHING on port 3000 (web/API/WebSocket/gamedata)
  · systemd unit: fafcn.service, install dir /opt/fafcn (owned by majiko)
```

| Owner | Scope |
|---|---|
| **Us (AliCloud CLI works locally)** | Domain renewal, DNS records (`aliyun` CLI profile `default`), code & deploys |
| **Friend (Lucky gateway)** | DDNS task, cert issuance/renewal, reverse-proxy rules, port forwards |
| **Friend's DNS key** | AliCloud **RAM sub-account** scoped to `faforever.cn` DNS only (policy on `acs:alidns:*:*:domain/faforever.cn`). If leaked: revoke in the RAM console and reissue. **Never hand out the main-account or local CLI AccessKey** |

## 3. Deploy & ops commands (repo root)

```bash
cargo xtask fafcn majiko-deploy              # full deploy (backend + wasm plugin + web)
cargo xtask fafcn majiko-deploy --skip-web   # backend only
cargo xtask fafcn majiko-deploy-file-sync    # fafcn-sync Windows client only
cargo xtask fafcn majiko-health              # three-layer check (SSH → service → public); first troubleshooting step
```

- Deploy config: `xtask/.env` (git-ignored) — `MAJIKO_SSH_PASSWORD` (SSH/sudo password in plaintext here), `MAJIKO_PUBLIC_URL=https://faforever.cn:60`
- SSH: `ssh -p 10040 majiko@8v.pub` (same password; sudo uses it too)
- Server logs: `journalctl -u fafcn -f` or `/opt/fafcn/data/logs/fafcn-server.log`

## 4. Secrets index (locations only)

| Secret | Location |
|---|---|
| majiko SSH/sudo password | local `xtask/.env` |
| LLM API key (secsino relay, friend's account) | server `/opt/fafcn/.env` (`FAFCN_LLM_*`) |
| gamedata upload token (UPLOAD_TOKEN) | server `/opt/fafcn/.env` |
| FAF OAuth client_id/secret (**pending**) | when received: server `/opt/fafcn/.env` + local `apps/fafcn-server/.env` |
| Old Kimi LLM config backup | server `/opt/fafcn/.env.bak-kimi` (rollback-ready) |
| AliCloud CLI AK (this machine) | `aliyun configure list` (profile `default`) |

⚠️ The server `.env` and local `apps/fafcn-server/.env` are **out of sync**
(LLM was switched to the relay) — never blindly overwrite. Q&A is a public
feature: **every visitor question bills the friend's key**.

## 5. fafcn-sync client notes

- Player config: `%APPDATA%\fafcn-sync\config.toml` (server address etc.)
- The exe carries its download origin appended by the server (embedded
  config). Priority: **on the first run of a new build, the embedded origin
  wins** (detected via `last_build_tag` in config.toml — auto-repairs stale
  addresses left by old builds, e.g. the retired domain); afterwards the
  remembered address wins (four fixes on 2026-08-28/30: startup override /
  no save on close / hard-exit on self-update / stale legacy config)
- Self-update: hard `std::process::exit` exe swap (config is saved BEFORE
  the swap); new builds ship via `majiko-deploy-file-sync`, players click
  检查更新
- Upload flows show live progress in BOTH phases: local hashing
  (`正在计算本地文件校验和…`, byte-level, throttled) and transfer
- Current latest build: `dev-6a97f5c1-9a46`

## 6. FAF integration status

- OAuth application **approved** (Brutus5000, consent name `fafcn`),
  **credentials not yet received**
- FAF notified of the updated prod redirect URI:
  `https://faforever.cn:60/api/auth/callback` (exact port whitelist)
- Domain concern resolved: FAF asked that `faforever.cn` not look official →
  disclaimer banner on the home page + global footer added (§2.3 of
  `faf-integration.md`)
- When credentials arrive, follow the §2.1 checklist in
  `docs/fafcn/faf-integration.md`

## 7. Troubleshooting decision tree

1. `cargo xtask fafcn majiko-health` — see which layer is red
2. Service layer red → SSH in, `journalctl -u fafcn -n 50`
3. Public layer red, service green → friend's side: edge forward broken (LAN
   IP changed) / DDNS stale (public IP changed) / cert issue
4. Public port serves a self-signed `CN=Lucky` cert → the friend's Lucky
   vhost/cert binding for the domain is broken
5. `faforever.cn` doesn't resolve or resolves wrong → friend's DDNS task is
   down (or the domain expired)

## 8. Key documents

- Deploy runbook: `docs/fafcn/how_to_deploy_fafcn_on_majiko.md` (the detailed
  version of this note)
- FAF integration design: `docs/fafcn/faf-integration.md`
- File-sync architecture: `docs/fafcn/file-sync.md`
- bin channel (game binary) policy: `docs/fafcn/game-binary-channel.md`
- Generic fresh-server runbook: `docs/fafcn/how_to_deploy_fafcn.md`
