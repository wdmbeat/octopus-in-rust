use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cargo;
use crate::project;

/// The tailwindcss release tag pinned by dioxus-cli 0.7
/// (`TailwindCli::V4_TAG` in the CLI source). Bump this when the workspace
/// upgrades to a dioxus CLI that pins a different Tailwind release.
const DX_07_TAILWIND_TAG: &str = "v4.1.5";

/// Ensure the Dioxus toolchain is ready: the `dx` CLI matches the
/// major.minor of the `dioxus` crate pinned in the workspace `Cargo.toml`
/// (installing/upgrading it with `cargo install` when needed), and the app's
/// `assets/tailwind.css` can be produced.
///
/// Fresh clones need this: `assets/tailwind.css` is gitignored (generated),
/// so a plain `cargo build`/`cargo clippy` — or an older dx — fails with
/// "Asset at /assets/tailwind.css doesn't exist". A CLI/crate major.minor
/// mismatch also makes `dx serve` / `dx build` misbehave.
///
/// `app_dir` is the Dioxus app directory (the one containing `tailwind.css`),
/// e.g. `apps/fafcn-web`.
pub fn ensure_toolchain(app_dir: &Path) -> Result<()> {
    ensure_cli()?;
    ensure_tailwind(app_dir)
}

fn ensure_cli() -> Result<()> {
    let required = required_dioxus_version()?;
    match installed_dx_version() {
        Some(installed) if installed == required => Ok(()),
        found => {
            match found {
                Some(installed) => println!(
                    "dx {}.{}.x found, but the workspace pins dioxus {}.{} — upgrading the CLI...",
                    installed.0, installed.1, required.0, required.1
                ),
                None => println!(
                    "dx (dioxus-cli) not found — installing {}.{} to match the workspace...",
                    required.0, required.1
                ),
            }
            install_cli(required)?;
            match installed_dx_version() {
                Some(v) if v == required => Ok(()),
                Some(v) => bail!(
                    "installed dx {}.{}.x, but {}.{} is required — check PATH for a stale dx binary",
                    v.0, v.1, required.0, required.1
                ),
                None => bail!(
                    "dx is still not found after install — make sure ~/.cargo/bin is on PATH"
                ),
            }
        }
    }
}

/// Make sure the app's `assets/tailwind.css` can be produced, in order:
///
/// 1. If dx's managed tailwindcss binary is present, dx compiles the CSS
///    itself before every build — nothing to do.
/// 2. Otherwise compile the CSS right now with the npm-published Tailwind
///    CLI (`@tailwindcss/cli`, same engine as the standalone binary), cached
///    under the gitignored workspace `target/` dir. This is the fallback for
///    networks where GitHub release assets stall: dx downloads its binary
///    from GitHub on first use and silently ignores failures
///    (`_ = TailwindCli::run_once(...)` in its build prebuild), which later
///    surfaces as the confusing `asset!("/assets/tailwind.css") doesn't
///    exist` compile error.
/// 3. Last resort for machines without node: fetch the official standalone
///    binary from GitHub ourselves (the same URL dx would use) and let dx
///    compile the CSS.
///
/// If all three fail, bail loudly instead of letting the build die on the
/// asset macro.
fn ensure_tailwind(app_dir: &Path) -> Result<()> {
    if tailwind_binary_path()?.exists() {
        return Ok(());
    }

    if command_available("node") && command_available("npm") {
        match generate_tailwind_css_via_npm(app_dir) {
            Ok(()) => return Ok(()),
            Err(e) => eprintln!(
                "npm-based tailwind build failed ({e:#}); trying the GitHub binary download..."
            ),
        }
    }

    install_tailwind_via_curl(&tailwind_binary_path()?)
}

/// Compile `<app_dir>/assets/tailwind.css` from `<app_dir>/tailwind.css`
/// using the npm-published Tailwind CLI. `NODE_PATH` is required: the app
/// CSS does `@import "tailwindcss"`, which the CLI resolves from the CSS
/// file's directory upwards, and the app's `node_modules` does not exist on
/// a fresh clone.
fn generate_tailwind_css_via_npm(app_dir: &Path) -> Result<()> {
    let node_modules = ensure_npm_tailwind()?;
    let cli = node_modules.join("@tailwindcss/cli/dist/index.mjs");

    println!("Compiling {}/assets/tailwind.css via npm's @tailwindcss/cli...", app_dir.display());
    let status = Command::new("node")
        .arg(&cli)
        .args(["--input", "tailwind.css", "--output", "assets/tailwind.css"])
        .current_dir(app_dir)
        .env("NODE_PATH", &node_modules)
        .status()
        .context("failed to spawn node")?;
    if !status.success() {
        bail!("tailwindcss CLI exited with status: {status}");
    }

    let output = app_dir.join("assets").join("tailwind.css");
    if !output.is_file() {
        bail!("tailwindcss ran but {} was not produced", output.display());
    }
    println!("Wrote {}", output.display());
    Ok(())
}

/// Install `@tailwindcss/cli` + `tailwindcss` into a workspace-local cache
/// dir (`target/xtask-tailwind`, gitignored) and return its `node_modules`.
fn ensure_npm_tailwind() -> Result<PathBuf> {
    let version = DX_07_TAILWIND_TAG.trim_start_matches('v');
    let dir = project::root().join("target").join("xtask-tailwind");
    let node_modules = dir.join("node_modules");
    if node_modules
        .join("@tailwindcss/cli/dist/index.mjs")
        .is_file()
    {
        return Ok(node_modules);
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    std::fs::write(
        dir.join("package.json"),
        "{\"name\":\"xtask-tailwind\",\"private\":true}\n",
    )
    .context("failed to write package.json")?;

    println!("Installing @tailwindcss/cli@{version} via npm (cached in {})...", dir.display());
    let status = Command::new("npm")
        .args([
            "install",
            "--no-audit",
            "--no-fund",
            &format!("@tailwindcss/cli@{version}"),
            &format!("tailwindcss@{version}"),
        ])
        .current_dir(&dir)
        .status()
        .context("failed to spawn npm")?;
    if !status.success() {
        bail!("npm install of tailwindcss@{version} failed ({status})");
    }
    Ok(node_modules)
}

/// Fetch the official standalone binary from GitHub releases — the same URL
/// dx itself uses (`TailwindCli::git_install_url` in the CLI source).
fn install_tailwind_via_curl(binary: &Path) -> Result<()> {
    let target = tailwind_download_target()?;
    let url = format!(
        "https://github.com/tailwindlabs/tailwindcss/releases/download/{DX_07_TAILWIND_TAG}/tailwindcss-{target}"
    );
    println!("Downloading the tailwindcss binary dx needs ({DX_07_TAILWIND_TAG}, {target})...");

    let parent = binary.parent().context("tailwind binary path has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    let tmp = binary.with_extension("download");
    let status = Command::new("curl")
        .arg("-fSL")
        .args(["--connect-timeout", "10", "--max-time", "120", "--retry", "2", "-o"])
        .arg(&tmp)
        .arg(&url)
        .status()
        .context("failed to spawn curl")?;
    if !status.success() {
        _ = std::fs::remove_file(&tmp);
        bail!(
            "could not obtain tailwindcss: dx's managed binary is missing, the\n  \
             npm fallback was unavailable, and downloading {url} failed.\n  \
             Fix one of them: install Node.js (npm fallback), or make GitHub\n  \
             release downloads reachable and retry."
        );
    }
    std::fs::rename(&tmp, binary)
        .with_context(|| format!("failed to move {} into place", binary.display()))?;
    chmod_executable(binary)?;

    println!("Installed tailwindcss to {}", binary.display());
    Ok(())
}

/// Where dx 0.7 looks for its managed tailwindcss binary:
/// `$DX_HOME/tools/tailwindcss-<tag>/tailwindcss`, with DX_HOME defaulting
/// to ~/.dx on macOS/Windows and $XDG_DATA_HOME/.dx on Linux
/// (`Workspace::dioxus_data_dir` in the CLI source).
fn tailwind_binary_path() -> Result<PathBuf> {
    let base = match std::env::var_os("DX_HOME") {
        Some(path) => PathBuf::from(path),
        None => {
            let home = std::env::home_dir().context("could not resolve the home directory")?;
            if cfg!(any(target_os = "macos", target_os = "windows")) {
                home.join(".dx")
            } else {
                std::env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".local").join("share"))
                    .join(".dx")
            }
        }
    };
    let name = if cfg!(windows) {
        "tailwindcss.exe"
    } else {
        "tailwindcss"
    };
    Ok(base
        .join("tools")
        .join(format!("tailwindcss-{DX_07_TAILWIND_TAG}"))
        .join(name))
}

/// The platform triple used in tailwindcss GitHub release asset names
/// (mirrors `TailwindCli::downloaded_bin_name` in the CLI source).
fn tailwind_download_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("linux", "x86_64") => Ok("linux-x64"),
        // tailwind does not distribute Windows arm64 binaries
        ("windows", _) => Ok("windows-x64.exe"),
        (os, arch) => bail!("no tailwindcss standalone binary for {os}/{arch}"),
    }
}

#[cfg(unix)]
fn chmod_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = path.metadata()?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn chmod_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn command_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The `dioxus` version pinned in `[workspace.dependencies]`.
fn required_dioxus_version() -> Result<(u64, u64)> {
    let manifest = project::root().join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("failed to read {}", manifest.display()))?;
    // NOTE: parse as a document (Table), not toml::Value — in toml 1.x,
    // Value::from_str parses a single TOML value and rejects documents.
    let doc: toml::Table = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", manifest.display()))?;
    let version = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.get("dioxus"))
        .and_then(|d| d.get("version"))
        .and_then(|v| v.as_str())
        .context("workspace Cargo.toml does not pin workspace.dependencies.dioxus.version")?;
    parse_major_minor(version)
        .with_context(|| format!("unparseable dioxus version requirement '{version}'"))
}

/// `dx --version` prints e.g. "dioxus 0.7.9 (was built without git repository)".
fn installed_dx_version() -> Option<(u64, u64)> {
    let output = Command::new("dx").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout.split_whitespace().nth(1)?;
    parse_major_minor(version)
}

/// Install the matching dioxus-cli with cargo. Compiling from source takes
/// a few minutes on first install; `--force` replaces a stale `dx` binary.
fn install_cli(required: (u64, u64)) -> Result<()> {
    let version_req = format!("^{}.{}", required.0, required.1);
    println!("Running: cargo install dioxus-cli --locked --version '{version_req}' --force");
    println!("(this compiles the CLI from source and can take a few minutes)");
    let mut cmd = cargo::command();
    cmd.args([
        "install",
        "dioxus-cli",
        "--locked",
        "--version",
        &version_req,
        "--force",
    ]);
    cargo::run(&mut cmd).context("failed to install dioxus-cli")?;
    Ok(())
}

fn parse_major_minor(version: &str) -> Option<(u64, u64)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_major_minor() {
        assert_eq!(parse_major_minor("0.7"), Some((0, 7)));
        assert_eq!(parse_major_minor("0.7.9"), Some((0, 7)));
        assert_eq!(parse_major_minor("1.2.3-rc.1"), Some((1, 2)));
        assert_eq!(parse_major_minor(" 0.6.1 "), Some((0, 6)));
        assert_eq!(parse_major_minor("0"), None);
        assert_eq!(parse_major_minor("x.y"), None);
        assert_eq!(parse_major_minor(""), None);
    }
}
