//! Deploy and inspect the fafcn stack on the `majiko` home server (`8v.pub`).
//!
//! Commands (the `majiko-*` family):
//!
//! ```text
//! cargo xtask fafcn majiko-deploy [--skip-web] [--with-gamedata] [--skip-verify]
//! cargo xtask fafcn majiko-health
//! cargo xtask fafcn majiko-deploy-file-sync   (sync client binary only)
//! ```
//!
//! Connection details and secrets come from (highest priority first):
//! process environment, then `xtask/.env` (git-ignored — see
//! `xtask/.env.example`), then the built-in defaults below.
//!
//! Reference runbook: `docs/deploy_fafcn/howto_deploy_to_majiko.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cargo;

/// SSH login user on the majiko server.
const DEFAULT_USER: &str = "majiko";
/// Public host of the majiko server.
const DEFAULT_HOST: &str = "8v.pub";
/// External SSH port forwarded to the server.
const DEFAULT_SSH_PORT: u16 = 10040;
/// Install directory on the server (owned by the SSH user).
const DEFAULT_DEPLOY_DIR: &str = "/opt/fafcn";
/// Public base URL of the deployed site (friend's TLS reverse proxy →
/// edge forward → `192.168.50.10:3000`). Moved from `https://8v.pub:10041`
/// to the AliCloud-registered domain on 2026-08-28.
const DEFAULT_PUBLIC_URL: &str = "https://faforever.cn:60";
/// systemd unit name on the server.
const SERVICE_NAME: &str = "fafcn";

/// Deployment target configuration for the majiko server.
#[derive(Debug, Clone)]
struct MajikoConfig {
    user: String,
    host: String,
    ssh_port: u16,
    deploy_dir: String,
    public_url: String,
    ssh_password: String,
}

impl MajikoConfig {
    /// Load configuration: process env > `xtask/.env` > defaults.
    fn load() -> Result<Self> {
        let env_file = EnvFile::load()?;
        let get = |key: &str, default: &str| -> String {
            std::env::var(key)
                .ok()
                .or_else(|| env_file.get(key))
                .unwrap_or_else(|| default.to_string())
        };

        let ssh_password = std::env::var("MAJIKO_SSH_PASSWORD")
            .ok()
            .or_else(|| env_file.get("MAJIKO_SSH_PASSWORD"))
            .context(
                "MAJIKO_SSH_PASSWORD is not set. Add it to xtask/.env \
                 (see xtask/.env.example) or export it in the environment.",
            )?;

        Ok(Self {
            user: get("MAJIKO_SSH_USER", DEFAULT_USER),
            host: get("MAJIKO_SSH_HOST", DEFAULT_HOST),
            ssh_port: get("MAJIKO_SSH_PORT", &DEFAULT_SSH_PORT.to_string())
                .parse()
                .context("MAJIKO_SSH_PORT must be a port number")?,
            deploy_dir: get("MAJIKO_DEPLOY_DIR", DEFAULT_DEPLOY_DIR),
            public_url: get("MAJIKO_PUBLIC_URL", DEFAULT_PUBLIC_URL),
            ssh_password,
        })
    }

    /// `ssh` command prefixed with sshpass, ready to take a remote command.
    fn ssh_base(&self) -> Command {
        let mut cmd = Command::new("sshpass");
        cmd.arg("-p").arg(&self.ssh_password);
        cmd.arg("ssh")
            .arg("-p")
            .arg(self.ssh_port.to_string())
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg(format!("{}@{}", self.user, self.host));
        cmd
    }

    /// Run a remote command, streaming its output; fail on non-zero exit.
    fn ssh(&self, remote_cmd: &str) -> Result<()> {
        let status = self
            .ssh_base()
            .arg(remote_cmd)
            .status()
            .context("failed to spawn ssh")?;
        if !status.success() {
            bail!("remote command failed ({status}): {remote_cmd}");
        }
        Ok(())
    }

    /// Run a remote command and capture stdout; fail on non-zero exit.
    fn ssh_output(&self, remote_cmd: &str) -> Result<String> {
        let output = self
            .ssh_base()
            .arg(remote_cmd)
            .output()
            .context("failed to spawn ssh")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("remote command failed: {remote_cmd}\n{stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a remote command as root via `sudo -S` (password over stdin of the
    /// remote shell; the password never appears in the remote command line).
    fn ssh_sudo(&self, remote_cmd: &str) -> Result<()> {
        self.ssh(&format!(
            "echo '{}' | sudo -S -k {remote_cmd} 2>/dev/null",
            self.ssh_password
        ))
    }

    /// rsync a local path to a remote path under the deploy dir.
    fn rsync(&self, local: &str, remote: &str, extra: &[&str]) -> Result<()> {
        let ssh_cmd = format!(
            "sshpass -p '{}' ssh -p {} -o StrictHostKeyChecking=no",
            self.ssh_password, self.ssh_port
        );
        let dest = format!("{}@{}:{}", self.user, self.host, remote);
        let status = Command::new("rsync")
            .arg("-az")
            .args(extra)
            .arg("-e")
            .arg(&ssh_cmd)
            .arg(local)
            .arg(&dest)
            .status()
            .context("failed to spawn rsync")?;
        if !status.success() {
            bail!("rsync failed ({status}): {local} -> {dest}");
        }
        Ok(())
    }

    fn remote_path(&self, rel: &str) -> String {
        format!("{}/{}", self.deploy_dir, rel)
    }
}

/// Minimal parser for `xtask/.env` (KEY=VALUE lines, `#` comments).
struct EnvFile(HashMap<String, String>);

impl EnvFile {
    fn load() -> Result<Self> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");
        let mut map = HashMap::new();
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let line = line.strip_prefix("export ").unwrap_or(line);
                if let Some((key, value)) = line.split_once('=') {
                    map.insert(key.trim().to_string(), value.trim().to_string());
                }
            }
        }
        Ok(Self(map))
    }

    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

/// Parsed flags for `majiko-deploy`.
#[derive(Debug, Clone, Copy)]
struct DeployOptions {
    /// Skip the Dioxus frontend rebuild and web-dist sync.
    skip_web: bool,
    /// Also sync the ~800 MB gamedata mirror (normally server-managed).
    with_gamedata: bool,
    /// Skip the post-deploy health verification gates.
    skip_verify: bool,
}

impl DeployOptions {
    fn parse(rest: &[String]) -> Result<Self> {
        let mut opts = Self {
            skip_web: false,
            with_gamedata: false,
            skip_verify: false,
        };
        for arg in rest {
            match arg.as_str() {
                "--skip-web" => opts.skip_web = true,
                "--with-gamedata" => opts.with_gamedata = true,
                "--skip-verify" => opts.skip_verify = true,
                other => bail!("unknown majiko-deploy option '{other}'"),
            }
        }
        Ok(opts)
    }
}

/// Entry point for `cargo xtask fafcn majiko-deploy`.
pub fn run_deploy(rest: &[String]) -> Result<()> {
    let opts = DeployOptions::parse(rest)?;
    let cfg = MajikoConfig::load()?;

    println!("==> Target: {}@{}:{}", cfg.user, cfg.host, cfg.ssh_port);
    println!("==> Deploy dir: {}", cfg.deploy_dir);

    preflight(&cfg, &opts)?;
    build(&opts)?;
    ship(&cfg, &opts)?;
    restart(&cfg)?;
    if !opts.skip_verify {
        verify(&cfg)?;
    }

    println!();
    println!("✅ Deploy complete: {}", cfg.public_url);
    println!("   Ask the user to hard-refresh (Ctrl+Shift+R) if the web UI changed.");
    Ok(())
}

/// Check local tools and SSH connectivity before doing any work.
fn preflight(cfg: &MajikoConfig, opts: &DeployOptions) -> Result<()> {
    for tool in ["sshpass", "rsync"] {
        which(tool)?;
    }
    if !opts.skip_web {
        crate::dx::ensure_toolchain(Path::new("apps/fafcn-web"))?;
    }

    println!("==> Preflight: SSH connectivity...");
    let who = cfg
        .ssh_output("whoami")
        .context("SSH preflight failed — check MAJIKO_* settings in xtask/.env")?;
    if who != cfg.user {
        bail!("expected to log in as '{}', got '{who}'", cfg.user);
    }
    Ok(())
}

fn which(tool: &str) -> Result<()> {
    let ok = Command::new("which")
        .arg(tool)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        bail!("missing required tool '{tool}' on this machine");
    }
    Ok(())
}

/// Build every artifact that will be shipped.
fn build(opts: &DeployOptions) -> Result<()> {
    println!("==> Build: fafcn-server (release)...");
    let mut cmd = cargo::command();
    cmd.args(["build", "--release", "-p", "fafcn-server"]);
    cargo::run(&mut cmd).context("fafcn-server build failed")?;

    println!("==> Build: faf-units-plugin (wasm32, release)...");
    cargo::build_plugin("faf-units-plugin", true)?;

    if !opts.skip_web {
        println!("==> Build: fafcn-web (dx release)...");
        // A stale output dir can keep old hashed bundles around and has
        // shipped a DEBUG bundle once (runbook Pitfall 2) — always clean.
        let dist = PathBuf::from("target/dx/fafcn-web/release");
        if dist.exists() {
            std::fs::remove_dir_all(&dist)
                .with_context(|| format!("failed to clean {}", dist.display()))?;
        }
        let status = Command::new("dx")
            .args(["build", "--release", "--platform", "web"])
            .current_dir("apps/fafcn-web")
            .status()
            .context("failed to spawn dx")?;
        if !status.success() {
            bail!("dx build failed ({status})");
        }
        gate_web_bundle_is_release()?;
    }
    Ok(())
}

/// Runbook Pitfall 2 gate: a debug wasm bundle hardcodes `localhost:3000`
/// and breaks every API call in the browser. A true release bundle contains
/// exactly ONE occurrence (the unreachable `unwrap_or_else` fallback in
/// `api_base()`); more means debug assertions were on — refuse to ship.
fn gate_web_bundle_is_release() -> Result<()> {
    let assets = Path::new("target/dx/fafcn-web/release/web/public/assets");
    let mut wasm_files: Vec<PathBuf> = std::fs::read_dir(assets)
        .with_context(|| format!("failed to list {}", assets.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().ends_with(".wasm"))
                .unwrap_or(false)
        })
        .collect();
    wasm_files.sort();
    if wasm_files.len() != 1 {
        bail!(
            "expected exactly 1 wasm bundle in {}, found {}",
            assets.display(),
            wasm_files.len()
        );
    }
    let bytes = std::fs::read(&wasm_files[0])?;
    let needle = b"localhost:3000";
    let count = bytes.windows(needle.len()).filter(|w| *w == needle).count();
    if count != 1 {
        bail!(
            "web bundle looks like a DEBUG build (found {count} 'localhost:3000' \
             occurrences in {:?}, expected exactly 1). Refusing to ship — see \
             docs/deploy_fafcn/howto_deploy_to_majiko.md Pitfall 2.",
            wasm_files[0]
        );
    }
    println!("==> Gate: web bundle is a true release build (ok)");
    Ok(())
}

/// rsync all artifacts to the server.
fn ship(cfg: &MajikoConfig, opts: &DeployOptions) -> Result<()> {
    println!("==> Ship: server binary + wasm plugin...");
    cfg.rsync("target/release/fafcn-server", &cfg.remote_path("bin/"), &[])?;
    cfg.rsync(
        "target/wasm32-unknown-unknown/release/faf_units_plugin.wasm",
        &cfg.remote_path("data/qqbot-data/plugins/"),
        &[],
    )?;

    if !opts.skip_web {
        println!("==> Ship: web dist (rsync --delete)...");
        // --delete is mandatory: hashed asset names accumulate and stale
        // bundles must not survive (runbook Pitfall 2).
        cfg.rsync(
            "target/dx/fafcn-web/release/web/public/",
            &cfg.remote_path("web-dist/"),
            &["--delete"],
        )?;
    }

    println!("==> Ship: portraits + units file...");
    cfg.rsync("assets/icons/units", &cfg.remote_path("assets/icons/"), &[])?;
    cfg.rsync(
        "plugins/faf-units/data/faf_units.json",
        &cfg.remote_path("config/"),
        &[],
    )?;

    if opts.with_gamedata {
        println!("==> Ship: gamedata mirror (~800 MB, incremental)...");
        // NEVER --delete here: uploaded mirror content may exist only on
        // the server.
        cfg.rsync(
            "data/faf-gamedata/",
            &cfg.remote_path("data/faf-gamedata/"),
            &["--partial"],
        )?;
    }
    Ok(())
}

/// Entry point for `cargo xtask fafcn majiko-deploy-file-sync`.
///
/// Lightweight alternative to `majiko-deploy` when ONLY the fafcn-sync
/// Windows client changed: rebuild it (release, fresh build tag), rsync just
/// the client directory (`exe` + `VERSION`), and verify the status endpoint
/// reports the new tag. No service restart is needed — `download_client`
/// and `get_status` read these files per request.
pub fn run_file_sync_deploy() -> Result<()> {
    let cfg = MajikoConfig::load()?;
    println!("==> Target: {}@{}:{}", cfg.user, cfg.host, cfg.ssh_port);

    for tool in ["sshpass", "rsync"] {
        which(tool)?;
    }
    println!("==> Preflight: SSH connectivity...");
    let who = cfg
        .ssh_output("whoami")
        .context("SSH preflight failed — check MAJIKO_* settings in xtask/.env")?;
    if who != cfg.user {
        bail!("expected to log in as '{}', got '{who}'", cfg.user);
    }

    println!("==> Build: fafcn-sync (Windows, release)...");
    let tag = crate::apps::fafcn::build_file_sync(&[])?;

    println!("==> Ship: sync client binary...");
    cfg.rsync(
        "data/faf-gamedata/client/",
        &cfg.remote_path("data/faf-gamedata/client/"),
        &[],
    )?;

    println!(
        "==> Verify: build tag via public URL ({})...",
        cfg.public_url
    );
    let status = curl(&format!("{}/api/gamedata/status", cfg.public_url))?;
    if !status.contains(&tag) {
        bail!(
            "status endpoint does not report the new build tag {tag}: {}",
            truncate(&status)
        );
    }

    println!();
    println!("✅ Sync client {tag} is live at {}/sync", cfg.public_url);
    Ok(())
}

/// Restart the systemd service and confirm it is active.
fn restart(cfg: &MajikoConfig) -> Result<()> {
    println!("==> Restart: systemctl restart {SERVICE_NAME}...");
    cfg.ssh_sudo(&format!("systemctl restart {SERVICE_NAME}"))?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    let state = cfg.ssh_output(&format!("systemctl is-active {SERVICE_NAME}"))?;
    if state != "active" {
        bail!(
            "service is '{state}' after restart. Inspect with:\n  \
             journalctl -u {SERVICE_NAME} -n 50 --no-pager"
        );
    }
    Ok(())
}

/// Post-deploy health gates, on the server and through the public URL.
/// Uses the composite `/api/health` endpoint (standard service health +
/// Q&A LLM round-trip in one call).
fn verify(cfg: &MajikoConfig) -> Result<()> {
    println!("==> Verify: health on server (127.0.0.1:3000)...");
    let local = cfg.ssh_output("curl -s --max-time 60 http://127.0.0.1:3000/api/health")?;
    if !local.contains("\"status\":\"ok\"") {
        bail!("server-local health check failed: {local}");
    }

    println!("==> Verify: health via public URL ({})...", cfg.public_url);
    let public = curl(&format!("{}/api/health", cfg.public_url))?;
    if !public.contains("\"status\":\"ok\"") {
        bail!("public health check failed: {public}");
    }

    println!("==> Verify: gamedata status via public URL...");
    let status = curl(&format!("{}/api/gamedata/status", cfg.public_url))?;
    if !status.contains("\"channels\"") {
        bail!("gamedata status check failed: {status}");
    }
    Ok(())
}

fn curl(url: &str) -> Result<String> {
    // --noproxy '*': developer machines often have a local HTTP proxy in the
    // environment; the health gate must measure the real direct path to the
    // server, not the proxy's (a proxy failure would look like a site outage).
    let output = Command::new("curl")
        .args(["-s", "--noproxy", "*", "--max-time", "60", url])
        .output()
        .context("failed to spawn curl")?;
    if !output.status.success() {
        bail!("curl failed for {url}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// One layer of the `majiko-health` report.
enum LayerStatus {
    Ok(String),
    Fail(String),
}

impl LayerStatus {
    fn print(&self, layer: &str) -> bool {
        // Continuation lines align under the detail column.
        const INDENT: &str = "                 ";
        match self {
            LayerStatus::Ok(detail) => {
                println!(
                    "  ✅ {layer}: {}",
                    detail.replace('\n', &format!("\n{INDENT}"))
                );
                true
            }
            LayerStatus::Fail(detail) => {
                println!(
                    "  ❌ {layer}: {}",
                    detail.replace('\n', &format!("\n{INDENT}"))
                );
                false
            }
        }
    }
}

/// Parse the composite `/api/health` JSON body into polished detail lines.
/// Returns `(is_ok, detail)`; `None` if the body is not the expected shape.
fn health_detail(body: &str) -> Option<(bool, String)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let status = v.get("status")?.as_str()?;

    let svc = v.get("service")?;
    let units = svc.get("units_loaded")?.as_u64()?;
    let mark = |key: &str| {
        if svc.get(key).and_then(|x| x.as_bool()).unwrap_or(false) {
            "✓"
        } else {
            "✗"
        }
    };
    let svc_line = format!(
        "service: units={units} web-dist{} portraits{} gamedata{}",
        mark("web_dist_present"),
        mark("portraits_dir_present"),
        mark("gamedata_dir_present"),
    );

    let qa = v.get("qa")?;
    let qa_status = qa.get("status")?.as_str()?;
    let model = qa.get("model").and_then(|x| x.as_str()).unwrap_or("?");
    let base_url = qa.get("base_url").and_then(|x| x.as_str()).unwrap_or("?");
    let qa_line = if qa_status == "ok" {
        let reply = qa.get("reply").and_then(|x| x.as_str()).unwrap_or("?");
        format!("qa: ok — {model} via {base_url}, reply={reply:?}")
    } else {
        let error = qa.get("error").and_then(|x| x.as_str()).unwrap_or("?");
        format!("qa: ERROR — {model} via {base_url}: {}", truncate(error))
    };

    Some((status == "ok", format!("{svc_line}\n{qa_line}")))
}

/// Entry point for `cargo xtask fafcn majiko-health`.
///
/// Three layers, checked independently (a failure never aborts the later
/// layers, so the report shows exactly where the chain breaks):
///
/// 1. SSH reachability + login user.
/// 2. Service on the host: systemd state + `127.0.0.1:3000` health.
/// 3. Public deployment: `MAJIKO_PUBLIC_URL` health + gamedata status.
///
/// Exit code is non-zero if any layer failed.
pub fn run_health() -> Result<()> {
    let cfg = MajikoConfig::load()?;
    println!(
        "==> majiko health: {}@{}:{}",
        cfg.user, cfg.host, cfg.ssh_port
    );

    // Layer 1 — SSH.
    let ssh = match cfg.ssh_output("whoami") {
        Ok(who) if who == cfg.user => LayerStatus::Ok(format!("logged in as {who}")),
        Ok(who) => LayerStatus::Fail(format!("logged in as '{who}', expected '{}'", cfg.user)),
        Err(e) => LayerStatus::Fail(format!("SSH failed: {e:#}")),
    };
    let ssh_ok = ssh.print("SSH       ");

    // Layer 2 — service on the host (only meaningful if SSH works).
    let service = if !ssh_ok {
        LayerStatus::Fail("skipped (SSH unreachable)".to_string())
    } else {
        match cfg.ssh_output(&format!(
            "systemctl is-active {SERVICE_NAME}; curl -s --max-time 60 http://127.0.0.1:3000/api/health"
        )) {
            Ok(out) => {
                let mut lines = out.lines();
                let state = lines.next().unwrap_or("unknown");
                let health = lines.collect::<Vec<_>>().join(" ");
                match (state, health_detail(&health)) {
                    ("active", Some((true, detail))) => {
                        LayerStatus::Ok(format!("systemd active, /api/health ok\n{detail}"))
                    }
                    (_, Some((_, detail))) => LayerStatus::Fail(format!(
                        "systemd={state}, /api/health not ok\n{detail}"
                    )),
                    _ => LayerStatus::Fail(format!(
                        "systemd={state}, local health={}",
                        truncate(&health)
                    )),
                }
            }
            Err(e) => LayerStatus::Fail(format!("{e:#}")),
        }
    };
    let service_ok = service.print("Service   ");

    // Layer 3 — public deployment through the edge forward / reverse proxy.
    let public = if !service_ok {
        LayerStatus::Fail("skipped (service not healthy on host)".to_string())
    } else {
        let health_url = format!("{}/api/health", cfg.public_url);
        let status_url = format!("{}/api/gamedata/status", cfg.public_url);
        match (curl(&health_url), curl(&status_url)) {
            (Ok(h), Ok(s)) if s.contains("\"channels\"") => match health_detail(&h) {
                Some((true, detail)) => LayerStatus::Ok(format!(
                    "{} serves health + gamedata\n{}",
                    cfg.public_url,
                    detail.split('\n').next().unwrap_or_default()
                )),
                Some((false, detail)) => LayerStatus::Fail(format!(
                    "{} reachable but /api/health not ok\n{}",
                    cfg.public_url, detail
                )),
                None => LayerStatus::Fail(format!("unexpected health body: {}", truncate(&h))),
            },
            (Ok(_), Ok(s)) => {
                LayerStatus::Fail(format!("unexpected gamedata status body: {}", truncate(&s)))
            }
            (Err(e), _) | (_, Err(e)) => LayerStatus::Fail(format!(
                "{} unreachable from here: {e:#} \
                 (edge forward or friend's TLS proxy down?)",
                cfg.public_url
            )),
        }
    };
    let public_ok = public.print("Public    ");

    println!();
    if ssh_ok && service_ok && public_ok {
        println!("✅ all layers healthy");
        Ok(())
    } else {
        bail!("one or more layers unhealthy — see report above")
    }
}

fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(120).collect::<String>())
    } else {
        s.to_string()
    }
}
