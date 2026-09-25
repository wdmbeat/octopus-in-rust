//! faf-ml-datagen — synthetic training data for FAF strategic-icon detection.
//!
//! Instead of hand-labeling screenshots, composite the game's own
//! strategic-icon sprites onto crops of real (empty-terrain) screenshots.
//! Bounding boxes are known BY CONSTRUCTION, so every generated image comes
//! with perfect labels.
//!
//! Pipeline:
//!   sprites (DDS, 36×40 line art) → tint with a team color → scale to the
//!   on-screen size range → alpha-blend onto a random screenshot crop →
//!   record (class, x, y, w, h) per pasted sprite
//!
//! This crate is pure generation logic; it used to be the `faf-datagen` CLI.
//! Now `faf-ml-server` drives it: `POST /api/datagen` runs [`generate`] in a
//! background job, and the `on_sample` callback streams each sample into the
//! platform store (PNG + `LabeledBox` JSON — the store's JSON labels are the
//! single label format; the old YOLO text output was dropped with the CLI).
//!
//! ⚠ Domain-gap reminder: after generating, open a synthetic sample AND a
//! real screenshot side by side — the synthetic icons must match the real
//! render in SIZE, COLOR, and edge sharpness, or the model learns the wrong
//! object. And keep FUTURE screenshots held out as the real test set — never
//! train on the only real data you have.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use image::imageops::{crop_imm, overlay, resize, FilterType};
use image::{Rgba, RgbaImage};
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng}; // rand 0.10: random_range/random_bool live on RngExt

pub use faf_ml_core::DatagenConfig;

/// Team colors seen on the strategic map (approximate — tune against real
/// screenshots during the domain-gap check).
const TEAM_COLORS: [Rgba<u8>; 6] = [
    Rgba([240, 240, 240, 255]), // white / own
    Rgba([80, 220, 80, 255]),   // green (ally)
    Rgba([190, 80, 220, 255]),  // purple (enemy)
    Rgba([80, 200, 220, 255]),  // cyan
    Rgba([230, 210, 60, 255]),  // yellow
    Rgba([200, 200, 200, 255]), // grey (neutral)
];

/// One strategic-icon sprite: the `_rest` variant of one class.
pub struct Sprite {
    pub class_name: String,
    pub img: RgbaImage, // 36×40 with alpha
}

/// A placed unit: class NAME + pixel bounding box (absolute, on the sample).
/// The server maps these directly to platform `LabeledBox`es — no YOLO
/// roundtrip.
pub struct GenBox {
    pub class_name: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Loads every resting-state sprite (`*_rest.dds`, plus suffix-less icons),
/// decoding DDS → RGBA8. Class name = filename minus state suffix and an
/// optional `icon_` prefix: `icon_bomber1_directfire_rest.dds` →
/// `bomber1_directfire`, `SACU_RAS_rest.dds` → `SACU_RAS` (icon mods do not
/// all use the `icon_` prefix convention).
pub fn load_sprites(dir: &Path) -> Result<Vec<Sprite>> {
    let mut sprites = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {dir:?}"))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(base) = name.strip_suffix(".dds") else {
            continue;
        };
        let class_name = ["_selectedover", "_selected", "_over", "_rest"] // longest first
            .iter()
            .find_map(|suffix| base.strip_suffix(suffix))
            .unwrap_or(base);
        // Keep only the resting state (plus suffix-less strategic icons) —
        // the over/selected variants add UI markers and would double classes.
        if base != class_name && !name.ends_with("_rest.dds") {
            continue;
        }
        let class_name = class_name
            .strip_prefix("icon_")
            .unwrap_or(class_name)
            .to_ascii_lowercase();

        sprites.push(Sprite {
            class_name,
            img: decode_dds(&fs::read(&path)?, name)?,
        });
    }
    Ok(sprites)
}

/// Decode one DDS file's first mip into RGBA8.
fn decode_dds(bytes: &[u8], name: &str) -> Result<RgbaImage> {
    let dds = image_dds::ddsfile::Dds::read(&mut Cursor::new(bytes))
        .with_context(|| format!("parsing {name}"))?;
    image_dds::image_from_dds(&dds, 0).with_context(|| format!("decoding {name}"))
}

/// Load the resting-state sprite of ONE class (the inverse of the
/// name-stripping in `load_sprites`): tries `icon_{class}_rest.dds`,
/// `{class}_rest.dds` (prefix-less icon mods), then the suffix-less
/// variants — first with the exact class name, then CASE-INSENSITIVELY by
/// scanning the directory (mods mix cases between their assignment tables
/// and file names, e.g. class `mavor` vs `icon_Mavor_rest.dds`).
/// `Ok(None)` when nothing matches.
pub fn load_class_sprite(dir: &Path, class_name: &str) -> Result<Option<Sprite>> {
    for name in [
        format!("icon_{class_name}_rest.dds"),
        format!("{class_name}_rest.dds"),
        format!("icon_{class_name}.dds"),
        format!("{class_name}.dds"),
    ] {
        let path = dir.join(&name);
        if !path.is_file() {
            continue;
        }
        return Ok(Some(Sprite {
            class_name: class_name.to_string(),
            img: decode_dds(&fs::read(&path)?, &name)?,
        }));
    }
    // Case-insensitive fallback: find a file whose normalized class matches.
    for entry in fs::read_dir(dir).with_context(|| format!("reading {dir:?}"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(base) = name.strip_suffix(".dds") else {
            continue;
        };
        let stripped = ["_selectedover", "_selected", "_over", "_rest"]
            .iter()
            .find_map(|suffix| base.strip_suffix(suffix))
            .unwrap_or(base);
        // Skip non-resting state variants (same class with UI markers).
        if stripped.len() != base.len() && !base.ends_with("_rest") {
            continue;
        }
        let normalized = stripped
            .strip_prefix("icon_")
            .unwrap_or(stripped)
            .to_ascii_lowercase();
        if normalized == class_name {
            return Ok(Some(Sprite {
                class_name: class_name.to_string(),
                img: decode_dds(&fs::read(entry.path())?, &name)?,
            }));
        }
    }
    Ok(None)
}

/// Sorted, deduplicated class names of a sprite pool.
pub fn class_names(sprites: &[Sprite]) -> Vec<String> {
    let mut classes: Vec<String> = sprites.iter().map(|s| s.class_name.clone()).collect();
    classes.sort();
    classes.dedup();
    classes
}

/// Load sprites from several icon-set directories (e.g. enabled icon mods),
/// deduplicated by class name: a later directory's sprite OVERRIDES an
/// earlier one's for the same class — the same priority rule as in-game
/// mod loading. This lets a mod reskin a base class without renaming it.
pub fn load_sprites_multi(dirs: &[&Path]) -> Result<Vec<Sprite>> {
    let mut by_class: std::collections::HashMap<String, Sprite> = std::collections::HashMap::new();
    for dir in dirs {
        for sprite in load_sprites(dir)? {
            by_class.insert(sprite.class_name.clone(), sprite);
        }
    }
    Ok(by_class.into_values().collect())
}

/// Generate `config.count` samples, invoking `on_sample` per sample so the
/// caller can stream them into its store. Seeded by `config.seed` for
/// reproducibility. Empty sprite/background pools generate nothing — the
/// caller is expected to validate them first.
pub fn generate(
    config: &DatagenConfig,
    sprites: &[Sprite],
    backgrounds: &[RgbaImage],
    on_sample: &mut dyn FnMut(RgbaImage, Vec<GenBox>),
) {
    if sprites.is_empty() || backgrounds.is_empty() {
        return;
    }
    let mut rng = StdRng::seed_from_u64(config.seed);
    for _ in 0..config.count {
        let (img, boxes) = generate_sample(&mut rng, sprites, backgrounds, config);
        on_sample(img, boxes);
    }
}

// ── compositing ─────────────────────────────────────────────────────────────

/// Recolors a sprite to a team color: keep the alpha, scale the team color by
/// the source luminance (the sprites are grayscale line art the game tints).
fn tint(sprite: &RgbaImage, color: Rgba<u8>) -> RgbaImage {
    let mut out = sprite.clone();
    for Rgba([r, g, b, a]) in out.pixels_mut() {
        if *a == 0 {
            continue;
        }
        let lum = (*r as u32 + *g as u32 + *b as u32) as f32 / (3.0 * 255.0);
        *r = (color.0[0] as f32 * lum) as u8;
        *g = (color.0[1] as f32 * lum) as u8;
        *b = (color.0[2] as f32 * lum) as u8;
    }
    out
}

/// Generates one synthetic sample: random background crop + N tinted, scaled
/// sprites (with a clustering bias — real strategic views are clumpy).
fn generate_sample(
    rng: &mut impl RngExt,
    sprites: &[Sprite],
    backgrounds: &[RgbaImage],
    config: &DatagenConfig,
) -> (RgbaImage, Vec<GenBox>) {
    let bg = backgrounds.choose(rng).expect("non-empty background pool");
    let max_x = bg.width() - config.size;
    let max_y = bg.height() - config.size;
    let crop_x = rng.random_range(0..=max_x);
    let crop_y = rng.random_range(0..=max_y);
    let mut canvas = crop_imm(bg, crop_x, crop_y, config.size, config.size).to_image();

    let n_units = rng.random_range(1..=config.max_units);
    let mut boxes = Vec::with_capacity(n_units);
    // Cluster center for clumping (real games: armies move in blobs).
    let mut cluster: Option<(i64, i64)> = None;

    for _ in 0..n_units {
        let sprite = sprites.choose(rng).expect("non-empty sprite pool");
        let scale = rng.random_range(config.scale_min..config.scale_max);
        let w = ((sprite.img.width() as f32 * scale).round() as u32).max(2);
        let h = ((sprite.img.height() as f32 * scale).round() as u32).max(2);
        let icon = resize(
            &tint(&sprite.img, *TEAM_COLORS.choose(rng).unwrap()),
            w,
            h,
            FilterType::Lanczos3,
        );

        // 40% chance: place near the previous unit (cluster); else uniform.
        let (x, y) = match cluster {
            Some((cx, cy)) if rng.random_bool(0.4) => (
                (cx + rng.random_range(-40..=40)).clamp(0, config.size as i64 - w as i64),
                (cy + rng.random_range(-40..=40)).clamp(0, config.size as i64 - h as i64),
            ),
            _ => (
                rng.random_range(0..=(config.size - w) as i64),
                rng.random_range(0..=(config.size - h) as i64),
            ),
        };
        cluster = Some((x, y));

        overlay(&mut canvas, &icon, x, y);
        boxes.push(GenBox {
            class_name: sprite.class_name.clone(),
            x: x as u32,
            y: y as u32,
            w,
            h,
        });
    }
    (canvas, boxes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Workspace root (tests run with CWD = the crate dir).
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn load_sprites_normalizes_prefix_and_case() {
        let dir = workspace_root().join("tmp/Calibersexp/custom-strategic-icons");
        let sprites = load_sprites(&dir).unwrap();
        let names = class_names(&sprites);
        assert_eq!(names.len(), 18);
        assert!(names.contains(&"mavor".to_string()));
        assert!(names.contains(&"atlantis".to_string()));

        let dir = workspace_root().join("tmp/SACUIcons/custom-strategic-icons");
        let sprites = load_sprites(&dir).unwrap();
        let names = class_names(&sprites);
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"sacu_ras".to_string()));
    }

    #[test]
    fn load_class_sprite_handles_case_mismatch() {
        // Assignment tables say `icon_mavor`/`icon_Atlantis`, the files are
        // `icon_Mavor_rest.dds`/`icon_atlantis_rest.dds` — both must load.
        let dir = workspace_root().join("tmp/Calibersexp/custom-strategic-icons");
        assert!(load_class_sprite(&dir, "mavor").unwrap().is_some());
        assert!(load_class_sprite(&dir, "atlantis").unwrap().is_some());
        // Prefix-less mod (SACUIcons ships `SACU_RAS_rest.dds`).
        let dir = workspace_root().join("tmp/SACUIcons/custom-strategic-icons");
        assert!(load_class_sprite(&dir, "sacu_ras").unwrap().is_some());
        assert!(load_class_sprite(&dir, "nope").unwrap().is_none());
    }

    #[test]
    fn load_sprites_multi_later_dirs_override() {
        let root = workspace_root();
        let redux = root.join("tmp/ReduxStrategicIconsLarge/custom-strategic-icons");
        let calibers = root.join("tmp/Calibersexp/custom-strategic-icons");
        let sprites = load_sprites_multi(&[&redux, &calibers]).unwrap();
        let names = class_names(&sprites);
        // Redux classes survive; Calibersexp classes merge in; no dupes.
        assert!(names.contains(&"bomber1_directfire".to_string()));
        assert!(names.contains(&"mavor".to_string()));
        let total = names.len();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(total, deduped.len());
    }
}
