//! `download` subcommand — download and persist the FAF unit database.
//!
//! (This is the former `faf-downloader` crate, unchanged apart from being a
//! subcommand now.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use faf_units::FafUnitIndex;
use rusqlite::Connection;
use serde::Deserialize;
use tracing::info;

const DEFAULT_INDEX_URL: &str = "https://faforever.github.io/etfreeman-db/data/index.json";
const DEFAULT_TRANSLATIONS_PATH: &str = "crates/faf-units/data/zh_cn_units.json";

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Json,
    Sqlite,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// URL of the unit index JSON.
    #[arg(short, long, default_value = DEFAULT_INDEX_URL)]
    url: String,

    /// Output format.
    #[arg(short, long, value_enum, default_value = "json")]
    format: OutputFormat,

    /// Output file path.
    #[arg(short, long, default_value = "faf_units.json")]
    output: PathBuf,

    /// Pretty-print JSON output.
    #[arg(long)]
    pretty: bool,

    /// Path to a Simplified Chinese unit translation JSON file.
    #[arg(long, default_value = DEFAULT_TRANSLATIONS_PATH)]
    translations: PathBuf,
}

pub async fn run(cli: DownloadArgs) -> Result<()> {
    info!("Downloading FAF unit data from {}", cli.url);
    let index = download_index(&cli.url).await?;

    info!(
        "Loaded {} units (FAF version {})",
        index.units.len(),
        index.version
    );

    let mut index = index;
    match load_translations(&cli.translations) {
        Ok(translations) => {
            apply_translations(&mut index, &translations);
            info!(
                "Applied Chinese translations for {} units",
                translations.len()
            );
        }
        Err(e) => {
            tracing::warn!("Failed to load translations: {e}");
        }
    }

    match cli.format {
        OutputFormat::Json => write_json(&index, &cli.output, cli.pretty).await?,
        OutputFormat::Sqlite => write_sqlite(&index, &cli.output).await?,
    }

    info!("Wrote output to {}", cli.output.display());
    Ok(())
}

async fn download_index(url: &str) -> Result<FafUnitIndex> {
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("failed to GET {}", url))?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {} from {}", status, url);
    }

    let text = response
        .text()
        .await
        .context("failed to read response body")?;

    serde_json::from_str(&text).context("failed to parse FAF unit index")
}

#[derive(Debug, Clone, Deserialize)]
struct UnitTranslation {
    #[serde(default)]
    name: String,
    #[serde(default)]
    desc: String,
}

fn load_translations(path: &Path) -> Result<HashMap<String, UnitTranslation>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read translations from {}", path.display()))?;
    serde_json::from_str(&text).context("failed to parse translations JSON")
}

fn apply_translations(index: &mut FafUnitIndex, translations: &HashMap<String, UnitTranslation>) {
    for unit in &mut index.units {
        let key = unit.id.to_lowercase();
        if let Some(translation) = translations.get(&key) {
            if !translation.name.is_empty() {
                unit.name_zh = Some(translation.name.clone());
            }
            if !translation.desc.is_empty() {
                unit.description_zh = Some(translation.desc.clone());
            }
        }
    }
}

async fn write_json(index: &FafUnitIndex, path: &Path, pretty: bool) -> Result<()> {
    let output = if pretty {
        serde_json::to_string_pretty(index).context("failed to serialize pretty JSON")?
    } else {
        serde_json::to_string(index).context("failed to serialize JSON")?
    };

    tokio::fs::write(path, output)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

async fn write_sqlite(index: &FafUnitIndex, path: &Path) -> Result<()> {
    // SQLite access is synchronous; run it in a blocking task.
    let index = index.clone();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write_sqlite_sync(&index, &path))
        .await
        .context("sqlite write task panicked")?
}

fn write_sqlite_sync(index: &FafUnitIndex, path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove old {}", path.display()))?;
    }

    let mut conn =
        Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;

    conn.execute(
        "CREATE TABLE meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )
    .context("failed to create meta table")?;

    conn.execute(
        "CREATE TABLE units (
            id TEXT PRIMARY KEY,
            faction TEXT,
            name TEXT,
            description TEXT,
            categories TEXT NOT NULL,
            json TEXT NOT NULL
        )",
        [],
    )
    .context("failed to create units table")?;

    conn.execute("CREATE INDEX idx_units_faction ON units(faction)", [])
        .context("failed to create faction index")?;
    conn.execute("CREATE INDEX idx_units_name ON units(name)", [])
        .context("failed to create name index")?;

    let tx = conn.transaction().context("failed to begin transaction")?;
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO units (id, faction, name, description, categories, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .context("failed to prepare insert statement")?;

        for unit in &index.units {
            let faction = unit.faction().map(|s| s.to_string());
            let name = unit.name().map(|s| s.to_string());
            let categories = unit.categories.join(",");
            let json = serde_json::to_string(unit).context("failed to serialize unit")?;

            insert
                .execute((
                    &unit.id,
                    &faction,
                    &name,
                    &unit.description,
                    &categories,
                    &json,
                ))
                .with_context(|| format!("failed to insert unit {}", unit.id))?;
        }
    }
    tx.commit().context("failed to commit transaction")?;

    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('version', ?1)",
        [&index.version],
    )
    .context("failed to write version meta")?;

    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('unit_count', ?1)",
        [index.units.len().to_string()],
    )
    .context("failed to write unit_count meta")?;

    Ok(())
}
