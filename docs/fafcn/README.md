# fafcn Docs Index

Documentation for the fafcn web stack (website, gamedata mirror, fafcn-sync
client) and its deployment on the `majiko` home server. Public site:
**https://faforever.cn:60**

| File | Description |
|---|---|
| [majiko-maintenance-note.md](majiko-maintenance-note.md) | **Start here for ops.** Condensed maintenance summary: current URLs, architecture & responsibilities, secrets index (locations only), deploy commands, renewal reminders (domain expires 2027-08-25), troubleshooting decision tree. |
| [how_to_deploy_fafcn_on_majiko.md](how_to_deploy_fafcn_on_majiko.md) | The majiko deployment runbook (LLM-executable). Server facts, network/port-forward picture, full redeploy phases, routine update via `cargo xtask fafcn majiko-*`, LLM provider notes, domain/HTTPS details, known pitfalls. |
| [how_to_deploy_fafcn.md](how_to_deploy_fafcn.md) | Generic runbook for a **fresh server** (nginx + TLS, build-on-server option). Use when moving off majiko; the majiko doc above takes precedence for the current production host. |
| [file-sync.md](file-sync.md) | File-sync architecture & developer guide: mirror channels, the `FileSyncRule` per-file rule registry, sync rules, the fafcn-sync Windows client, auto-updater internals. Read before adding a channel or touching sync/update logic. |
| [faf-integration.md](faf-integration.md) | fafcn ↔ FAF integration design: OAuth2 login (status: approved, credentials pending), communication thread with the FAF team, credential checklist, community gallery design, rollout phases. |
| [game-binary-channel.md](game-binary-channel.md) | The `bin` channel: mirroring `ForgedAlliance.exe` so first-time players skip the official client's download. Includes FAF's exe distribution policy and our disclosure/gating plan — read before touching this channel. |
