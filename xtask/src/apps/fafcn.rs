use std::path::Path;

use anyhow::{Context, Result};

use crate::cargo;

/// Plugins the fafcn-server backend expects to find on disk.
const REQUIRED_PLUGINS: &[&str] = &["data/qqbot-data/plugins/faf_units_plugin.wasm"];

/// Directory the backend serves sync client binaries from
/// (default of `FAFCN_GAMEDATA_CLIENT_DIR`).
const SYNC_CLIENT_DIR: &str = "data/faf-gamedata/client";

/// Rust target triple for the Windows sync client. Players run Windows, so
/// this is the default (and only) target `file-sync` builds.
const SYNC_CLIENT_TARGET: &str = "x86_64-pc-windows-gnu";

/// File name the `/sync` page links to for the Windows client download.
const SYNC_CLIENT_FILE_NAME: &str = "fafcn-sync-x86_64-pc-windows-gnu.exe";

/// Run a fafcn-specific command.
pub fn run(command: &str, rest: &[String]) -> Result<()> {
    match command {
        "backend" => run_backend(),
        "frontend" => run_frontend(),
        "file-sync" => build_file_sync(rest).map(|_| ()),
        "majiko-deploy-file-sync" => crate::apps::fafcn_majiko::run_file_sync_deploy(),
        "unit-update" => update_units(),
        "majiko-deploy" => crate::apps::fafcn_majiko::run_deploy(rest),
        "majiko-health" => crate::apps::fafcn_majiko::run_health(),
        "help" | "-h" | "--help" => {
            crate::args::print_fafcn_help();
            Ok(())
        }
        other => {
            eprintln!("Unknown fafcn command '{}'", other);
            crate::args::print_fafcn_help();
            std::process::exit(1);
        }
    }
}

fn run_backend() -> Result<()> {
    ensure_plugins()?;

    let mut cmd = cargo::command();
    cmd.args(["run", "--package", "fafcn-server"]);

    // Keep dependency noise (reqwest, hyper, extism, wasmtime, rustls) at info/warn
    // while still showing debug output from the application crates.  Respect any
    // RUST_LOG the user already has set.
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env(
            "RUST_LOG",
            "info,fafcn_server=debug,agent_core=debug,llm_provider=debug,reqwest=warn,hyper=warn,hyper_util=warn,rustls=warn,extism=warn,wasmtime=warn",
        );
    }

    println!("Starting fafcn backend...");
    let mut child = cmd.spawn().context("failed to spawn fafcn-server")?;
    println!("Server PID: {}", child.id());

    let status = child.wait().context("failed to wait for fafcn-server")?;
    if !status.success() {
        anyhow::bail!("fafcn-server exited with status: {status}");
    }
    Ok(())
}

fn run_frontend() -> Result<()> {
    let mut cmd = std::process::Command::new("dx");
    cmd.args(["serve", "--platform", "web", "--port", "8080"]);
    cmd.current_dir("apps/fafcn-web");

    println!("Starting fafcn frontend...");
    let status = cmd.status().context("failed to run dx serve")?;
    if !status.success() {
        anyhow::bail!("dx serve exited with status: {status}");
    }
    Ok(())
}

/// Path (repo-relative) of the unit database consumed by the server and
/// embedded into the WASM plugin at compile time.
const UNITS_JSON: &str = "plugins/faf-units/data/faf_units.json";

/// Refresh the unit database from the upstream ETFreeman mirror
/// (`faf-unit-tools download` fetches it and merges the zh-CN translations).
///
/// This only rewrites the JSON file. Nothing picks it up automatically:
/// the server reads it at startup and the WASM plugin embeds it at compile
/// time, so the follow-up commands are printed explicitly at the end.
fn update_units() -> Result<()> {
    println!("Updating {UNITS_JSON} via faf-unit-tools...");
    let mut cmd = cargo::command();
    cmd.args([
        "run",
        "--release",
        "-p",
        "faf-unit-tools",
        "--",
        "download",
        "-f",
        "json",
        "-o",
        UNITS_JSON,
    ]);
    cargo::run(&mut cmd).context("faf-unit-tools download failed")?;

    // Sanity-check the freshly written file and report what we got.
    let text = std::fs::read_to_string(UNITS_JSON)
        .with_context(|| format!("failed to read back {UNITS_JSON}"))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("{UNITS_JSON} is not valid JSON"))?;
    let version = parsed
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let units = parsed
        .get("units")
        .and_then(|u| u.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if units == 0 {
        anyhow::bail!("{UNITS_JSON} contains 0 units — refusing to treat this as success");
    }
    println!("Updated: {units} units, FAF version {version}");

    println!();
    println!("The JSON is refreshed, but nothing uses it yet. Next steps:");
    println!();
    println!("  1. Rebuild the WASM plugin (it embeds the JSON at compile time):");
    println!("       cargo build --release -p faf-units-plugin --target wasm32-unknown-unknown");
    println!("       cp target/wasm32-unknown-unknown/release/faf_units_plugin.wasm data/qqbot-data/plugins/");
    println!();
    println!("  2. Deploy everything to the majiko server (ships the JSON + rebuilt");
    println!("     plugin and restarts the service):");
    println!("       cargo xtask fafcn majiko-deploy");
    println!();
    println!("  3. If qqbot also uses the faf_units plugin, restart it so it picks up");
    println!("     the rebuilt wasm from data/qqbot-data/plugins/:");
    println!("       cargo xtask qqbot restart   # or: qqbot tools update");
    Ok(())
}

/// Cross-compile the `fafcn-sync` CLI for Windows and install it where the
/// backend serves it, so the `/sync` page download link hands players a real
/// Windows binary. Returns the fresh build tag.
///
/// Release is the default: debug builds keep a console window (see the
/// `windows_subsystem` gate in fafcn-sync's main.rs), which is exactly what
/// we do NOT want to publish to non-technical players.
///
/// Every build is stamped with a fresh tag (compiled into the exe AND
/// written to a VERSION file the status endpoint serves), so users can
/// verify the /sync page and their download match.
pub(crate) fn build_file_sync(rest: &[String]) -> Result<String> {
    let release = !rest.iter().any(|a| a == "--debug");
    ensure_windows_cross_toolchain()?;
    let tag = new_build_tag();

    let mut cmd = cargo::command();
    cmd.args([
        "build",
        "--package",
        "fafcn-sync",
        "--target",
        SYNC_CLIENT_TARGET,
    ]);
    if release {
        cmd.arg("--release");
    }
    cmd.env("FAFCN_SYNC_BUILD_TAG", &tag);
    println!(
        "Building fafcn-sync for {SYNC_CLIENT_TARGET} ({}) with tag {tag}...",
        crate::project::profile_str(release)
    );
    cargo::run(&mut cmd).context("fafcn-sync build failed")?;

    let profile = crate::project::profile_str(release);
    let built = format!("target/{SYNC_CLIENT_TARGET}/{profile}/fafcn-sync.exe");
    let dest_dir = Path::new(SYNC_CLIENT_DIR);
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;
    let dest = dest_dir.join(SYNC_CLIENT_FILE_NAME);
    std::fs::copy(&built, &dest)
        .with_context(|| format!("failed to copy {built} to {}", dest.display()))?;
    std::fs::write(dest_dir.join("VERSION"), format!("{tag}\n"))
        .context("failed to write VERSION file")?;

    println!("Installed {built} -> {}", dest.display());
    println!("Build tag: {tag} (shown on the /sync page and in the client title bar)");
    Ok(tag)
}

/// A unique-per-build tag: timestamp + random suffix, e.g. `dev-68f3a1c2-9b4e`.
fn new_build_tag() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let rand = RandomState::new().build_hasher().finish();
    format!("dev-{:08x}-{:04x}", secs as u32, (rand as u16))
}

/// Verify the Rust Windows target and the MinGW linker are installed.
fn ensure_windows_cross_toolchain() -> Result<()> {
    let output = std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("failed to run `rustup target list --installed`")?;
    let installed = String::from_utf8_lossy(&output.stdout);
    if !installed.lines().any(|t| t.trim() == SYNC_CLIENT_TARGET) {
        anyhow::bail!(
            "missing Rust target {SYNC_CLIENT_TARGET}. Install it with:\n  \
             rustup target add {SYNC_CLIENT_TARGET}"
        );
    }

    let linker_ok = std::process::Command::new("x86_64-w64-mingw32-gcc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !linker_ok {
        anyhow::bail!(
            "missing the MinGW cross linker. Install it with:\n  \
             sudo apt install gcc-mingw-w64-x86-64"
        );
    }
    Ok(())
}

/// Verify that all WASM plugins required by the backend exist.
fn ensure_plugins() -> Result<()> {
    let mut missing = Vec::new();
    for path in REQUIRED_PLUGINS {
        if !Path::new(path).is_file() {
            missing.push(*path);
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    eprintln!("Missing WASM plugin(s) required by the fafcn-server backend:");
    for path in &missing {
        eprintln!("  - {path}");
    }
    eprintln!();
    eprintln!("Build and install them with:");
    eprintln!("  cargo build --release -p faf-units-plugin --target wasm32-unknown-unknown");
    eprintln!("  mkdir -p data/qqbot-data/plugins");
    eprintln!(
        "  cp target/wasm32-unknown-unknown/release/faf_units_plugin.wasm data/qqbot-data/plugins/"
    );

    anyhow::bail!("missing required plugins");
}
