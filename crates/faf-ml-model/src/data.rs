//! Dataset loaders. Three on-disk layouts are supported:
//!   1. YOLO datagen dir (backwards compatibility): images/000000.png,
//!      labels/000000.txt (YOLO: `<class_id> <cx> <cy> <w> <h>` normalized),
//!      classes.txt (line number = class id).
//!   2. The faf-ml platform store: screenshots/<uuid>.png + index.json,
//!      labels/<uuid>.json ([LabeledBox], absolute pixels), classes.txt.
//!      Only `synthetic`-kind screenshots become training samples.
//!   3. A dataset SNAPSHOT (`load_snapshot`): datasets/<name>.json embeds
//!      image ids + labels; images/dims resolve through the store. This is
//!      what training uses — snapshots are immutable, so runs reproduce.
//!
//! Images are decoded lazily per batch from the host (thousands of 640×640
//! frames don't fit one GPU buffer — the Fashion-MNIST lesson); anchor
//! targets are computed on the host in plain Rust and uploaded as tensors.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use burn::tensor::backend::Backend;
use burn::tensor::{Device, Int, Tensor, TensorData};
use faf_ml_core::{LabeledBox, ScreenshotKind, ScreenshotMeta};

use crate::anchors::CenterBox;
use crate::matching::AnchorTargets;

/// Positive-match IoU threshold (plan: pos if IoU ≥ 0.5; every GT also
/// force-matched to its best anchor — see `matching::match_anchors`).
pub const POS_IOU_THRESHOLD: f32 = 0.5;

/// One ground-truth box of one image: 0-based class id + normalized box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GtBox {
    pub class_id: usize,
    pub bbox: CenterBox,
}

/// One dataset sample: image path + its parsed labels.
#[derive(Debug, Clone)]
pub struct Sample {
    pub image_path: PathBuf,
    pub gt: Vec<GtBox>,
}

/// A dataset directory, loaded (labels eagerly, images lazily).
#[derive(Debug, Clone)]
pub struct DetectDataset {
    pub classes: Vec<String>,
    pub samples: Vec<Sample>,
    pub input_size: u32,
}

/// One assembled training batch on the target device.
pub struct TrainBatch<B: Backend> {
    pub images: Tensor<B, 4>,
    pub cls_targets: Tensor<B, 2, Int>,
    pub box_targets: Tensor<B, 3>,
    pub pos_mask: Tensor<B, 2>,
}

impl DetectDataset {
    /// Load a dataset directory, dispatching on layout: a dir containing
    /// `screenshots/index.json` is the platform store ([`Self::load_store`]);
    /// anything else is treated as a YOLO datagen dir ([`Self::load_yolo_dir`]).
    pub fn load(dir: &Path, input_size: u32) -> Result<Self> {
        if dir.join("screenshots").join("index.json").is_file() {
            Self::load_store(dir, input_size)
        } else {
            Self::load_yolo_dir(dir, input_size)
        }
    }

    /// Load the faf-ml platform store: `synthetic`-kind screenshots from
    /// `screenshots/`, their `labels/<uuid>.json` (absolute-pixel
    /// `LabeledBox`es normalized by the meta dims), and `classes.txt`.
    pub fn load_store(dir: &Path, input_size: u32) -> Result<Self> {
        let classes = read_classes(dir)?;

        let index_path = dir.join("screenshots").join("index.json");
        let raw = fs::read_to_string(&index_path)
            .with_context(|| format!("reading {}", index_path.display()))?;
        let metas: Vec<ScreenshotMeta> = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", index_path.display()))?;

        let mut samples = Vec::new();
        for meta in metas.iter().filter(|m| m.kind == ScreenshotKind::Synthetic) {
            let image_path = dir.join("screenshots").join(format!("{}.png", meta.id));
            anyhow::ensure!(
                image_path.exists(),
                "index entry {} has no image {}",
                meta.id,
                image_path.display()
            );
            let label_path = dir.join("labels").join(format!("{}.json", meta.id));
            let labels: Vec<LabeledBox> = match fs::read_to_string(&label_path) {
                Ok(raw) => serde_json::from_str(&raw)
                    .with_context(|| format!("parsing {}", label_path.display()))?,
                // A missing label file means an all-negative sample.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(err) => {
                    return Err(err).with_context(|| format!("reading {}", label_path.display()))
                }
            };
            let gt = labels
                .iter()
                .map(|b| labeled_box_to_gt(b, &classes, meta.width, meta.height))
                .collect::<Result<_>>()
                .with_context(|| format!("parsing {}", label_path.display()))?;
            samples.push(Sample { image_path, gt });
        }
        anyhow::ensure!(
            !samples.is_empty(),
            "no synthetic-kind screenshots in {}",
            dir.display()
        );

        Ok(Self {
            classes,
            samples,
            input_size,
        })
    }

    /// Load a dataset SNAPSHOT: `datasets/<name>.json` embeds image ids and
    /// their (immutable) labels; image files and dimensions resolve through
    /// the platform store in `dir`. Training input — a snapshot is a
    /// prerequisite for `train`.
    pub fn load_snapshot(dir: &Path, name: &str, input_size: u32) -> Result<Self> {
        let classes = read_classes(dir)?;

        let manifest_path = dir.join("datasets").join(format!("{name}.json"));
        let raw = fs::read_to_string(&manifest_path).with_context(|| {
            format!(
                "reading {} (no such snapshot — create one on the Datasets page first)",
                manifest_path.display()
            )
        })?;
        let manifest: faf_ml_core::DatasetManifest = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;

        // Snapshot entries carry labels but not image dims — index.json has them.
        let index_path = dir.join("screenshots").join("index.json");
        let raw = fs::read_to_string(&index_path)
            .with_context(|| format!("reading {}", index_path.display()))?;
        let metas: Vec<ScreenshotMeta> = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", index_path.display()))?;
        let dims: std::collections::HashMap<uuid::Uuid, (u32, u32)> =
            metas.iter().map(|m| (m.id, (m.width, m.height))).collect();

        let mut samples = Vec::with_capacity(manifest.entries.len());
        for entry in &manifest.entries {
            let image_path = dir
                .join("screenshots")
                .join(format!("{}.png", entry.image_id));
            let Some(&(width, height)) = dims.get(&entry.image_id) else {
                anyhow::bail!(
                    "snapshot {name:?} entry {} is missing from the store index",
                    entry.image_id
                );
            };
            anyhow::ensure!(
                image_path.exists(),
                "snapshot {name:?} entry {} has no image {} (was it deleted from the store?)",
                entry.image_id,
                image_path.display()
            );
            let gt = entry
                .labels
                .iter()
                .map(|b| labeled_box_to_gt(b, &classes, width, height))
                .collect::<Result<_>>()
                .with_context(|| format!("snapshot {name:?} entry {}", entry.image_id))?;
            samples.push(Sample { image_path, gt });
        }
        anyhow::ensure!(!samples.is_empty(), "snapshot {name:?} is empty");

        Ok(Self {
            classes,
            samples,
            input_size,
        })
    }

    /// Load `classes.txt` + `labels/*.txt`, pairing each label file with its
    /// `images/<stem>.png`. Label files sort lexicographically (zero-padded
    /// stems keep this numeric).
    pub fn load_yolo_dir(dir: &Path, input_size: u32) -> Result<Self> {
        let classes = read_classes(dir)?;

        let labels_dir = dir.join("labels");
        let mut label_files: Vec<PathBuf> = fs::read_dir(&labels_dir)
            .with_context(|| format!("reading {}", labels_dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("txt"))
            .collect();
        label_files.sort();

        let mut samples = Vec::with_capacity(label_files.len());
        for label_path in label_files {
            let stem = label_path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("label file without a valid stem")?
                .to_string();
            let image_path = dir.join("images").join(format!("{stem}.png"));
            anyhow::ensure!(
                image_path.exists(),
                "label {} has no image {}",
                label_path.display(),
                image_path.display()
            );
            let text = fs::read_to_string(&label_path)
                .with_context(|| format!("reading {}", label_path.display()))?;
            let gt = parse_yolo_labels(&text, classes.len())
                .with_context(|| format!("parsing {}", label_path.display()))?;
            samples.push(Sample { image_path, gt });
        }
        anyhow::ensure!(
            !samples.is_empty(),
            "no labels found in {}",
            labels_dir.display()
        );

        Ok(Self {
            classes,
            samples,
            input_size,
        })
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Assemble one training batch: decode the images, compute anchor targets
    /// on the host, upload everything to `device`.
    pub fn load_batch<B: Backend>(
        &self,
        indices: &[usize],
        anchors: &[CenterBox],
        device: &Device<B>,
    ) -> Result<TrainBatch<B>> {
        let batch = indices.len();
        let n_anchors = anchors.len();
        let side = self.input_size as usize;

        let mut pixels = Vec::with_capacity(batch * 3 * side * side);
        let mut cls = Vec::with_capacity(batch * n_anchors);
        let mut offsets = Vec::with_capacity(batch * n_anchors * 4);
        let mut mask = Vec::with_capacity(batch * n_anchors);

        for &i in indices {
            let sample = &self.samples[i];
            pixels.extend(decode_image(&sample.image_path, self.input_size)?);
            let gt: Vec<(usize, CenterBox)> =
                sample.gt.iter().map(|g| (g.class_id, g.bbox)).collect();
            let targets = AnchorTargets::build(anchors, &gt, POS_IOU_THRESHOLD);
            cls.extend(targets.cls);
            offsets.extend(targets.offsets.into_iter().flatten());
            mask.extend(targets.pos_mask);
        }

        Ok(TrainBatch {
            images: Tensor::from_data(TensorData::new(pixels, [batch, 3, side, side]), device),
            cls_targets: Tensor::from_data(TensorData::new(cls, [batch, n_anchors]), device),
            box_targets: Tensor::from_data(TensorData::new(offsets, [batch, n_anchors, 4]), device),
            pos_mask: Tensor::from_data(TensorData::new(mask, [batch, n_anchors]), device),
        })
    }
}

/// Read `classes.txt` (one class per line; line number = class id).
fn read_classes(dir: &Path) -> Result<Vec<String>> {
    let classes_text = fs::read_to_string(dir.join("classes.txt"))
        .with_context(|| format!("reading {}", dir.join("classes.txt").display()))?;
    let classes: Vec<String> = classes_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    anyhow::ensure!(
        !classes.is_empty(),
        "classes.txt is empty in {}",
        dir.display()
    );
    Ok(classes)
}

/// Convert one absolute-pixel platform label into a normalized center-form
/// GT box, resolving the class name against the class list.
fn labeled_box_to_gt(
    b: &LabeledBox,
    classes: &[String],
    image_width: u32,
    image_height: u32,
) -> Result<GtBox> {
    let class_id = classes
        .iter()
        .position(|c| c == &b.class)
        .with_context(|| format!("class {:?} not in classes.txt", b.class))?;
    let (w, h) = (image_width as f32, image_height as f32);
    Ok(GtBox {
        class_id,
        bbox: CenterBox {
            cx: (b.x + b.w / 2.0) / w,
            cy: (b.y + b.h / 2.0) / h,
            w: b.w / w,
            h: b.h / h,
        },
    })
}

/// Parse YOLO label text into normalized center-form GT boxes.
fn parse_yolo_labels(text: &str, num_classes: usize) -> Result<Vec<GtBox>> {
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        anyhow::ensure!(
            fields.len() == 5,
            "line {}: expected 5 fields <class_id> <cx> <cy> <w> <h>",
            lineno + 1
        );
        let class_id: usize = fields[0]
            .parse()
            .with_context(|| format!("line {}: bad class id", lineno + 1))?;
        anyhow::ensure!(
            class_id < num_classes,
            "line {}: class id {class_id} out of range ({num_classes} classes)",
            lineno + 1
        );
        let f = |i: usize| -> Result<f32> {
            fields[i]
                .parse()
                .with_context(|| format!("line {}: bad coordinate", lineno + 1))
        };
        out.push(GtBox {
            class_id,
            bbox: CenterBox {
                cx: f(1)?,
                cy: f(2)?,
                w: f(3)?,
                h: f(4)?,
            },
        });
    }
    Ok(out)
}

/// Decode one PNG into CHW f32 pixels, normalized to 0..=1 (datagen `/255`).
/// Fails loudly on size mismatch — the model is trained at a fixed input size.
fn decode_image(path: &Path, input_size: u32) -> Result<Vec<f32>> {
    let img = image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_rgb8();
    anyhow::ensure!(
        img.width() == input_size && img.height() == input_size,
        "{}: expected {}×{}, got {}×{}",
        path.display(),
        input_size,
        input_size,
        img.width(),
        img.height()
    );
    let raw = img.as_raw();
    let side = input_size as usize;
    let mut out = vec![0.0f32; 3 * side * side];
    for y in 0..side {
        for x in 0..side {
            let p = (y * side + x) * 3;
            for c in 0..3 {
                out[c * side * side + y * side + x] = raw[p + c] as f32 / 255.0;
            }
        }
    }
    Ok(out)
}

/// One image as a (1, 3, H, W) tensor on `device` (predict path).
pub fn image_tensor<B: Backend>(
    path: &Path,
    input_size: u32,
    device: &Device<B>,
) -> Result<Tensor<B, 4>> {
    let side = input_size as usize;
    let pixels = decode_image(path, input_size)?;
    Ok(Tensor::from_data(
        TensorData::new(pixels, [1, 3, side, side]),
        device,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yolo_parse_ok() {
        let gt = parse_yolo_labels("7 0.5 0.25 0.1 0.2\n12 0.0 1.0 0.05 0.05\n", 20).unwrap();
        assert_eq!(gt.len(), 2);
        assert_eq!(gt[0].class_id, 7);
        assert!((gt[0].bbox.cy - 0.25).abs() < 1e-6);
        assert_eq!(gt[1].class_id, 12);
    }

    #[test]
    fn yolo_parse_rejects_bad_input() {
        assert!(parse_yolo_labels("1 0.5 0.5 0.1", 20).is_err());
        assert!(parse_yolo_labels("99 0.5 0.5 0.1 0.1", 20).is_err());
        assert!(parse_yolo_labels("x 0.5 0.5 0.1 0.1", 20).is_err());
        assert!(parse_yolo_labels("1 0.5 0.5 0.1 z", 20).is_err());
    }

    #[test]
    fn labeled_box_converts_to_normalized_gt() {
        let classes = vec!["tank".to_string(), "bomber".to_string()];
        let b = LabeledBox {
            class: "bomber".to_string(),
            x: 100.0,
            y: 50.0,
            w: 40.0,
            h: 20.0,
        };
        let gt = labeled_box_to_gt(&b, &classes, 640, 640).unwrap();
        assert_eq!(gt.class_id, 1);
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(close(gt.bbox.cx, 120.0 / 640.0) && close(gt.bbox.cy, 60.0 / 640.0));
        assert!(close(gt.bbox.w, 40.0 / 640.0) && close(gt.bbox.h, 20.0 / 640.0));

        let unknown = LabeledBox {
            class: "ufo".to_string(),
            ..b
        };
        assert!(labeled_box_to_gt(&unknown, &classes, 640, 640).is_err());
    }

    #[test]
    fn snapshot_loads_embedded_labels_against_store_dims() {
        let dir =
            std::env::temp_dir().join(format!("octopus-test-snapshot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("screenshots")).unwrap();
        fs::create_dir_all(dir.join("datasets")).unwrap();
        fs::write(dir.join("classes.txt"), "tank\nbomber\n").unwrap();

        let id = uuid::Uuid::new_v4();
        // 1×1 placeholder (images decode lazily — loading never opens them).
        image::RgbaImage::new(1, 1)
            .save(dir.join("screenshots").join(format!("{id}.png")))
            .unwrap();
        let meta = ScreenshotMeta {
            id,
            filename: "s.png".to_string(),
            width: 640,
            height: 640,
            uploaded_at: chrono::Utc::now(),
            kind: ScreenshotKind::Synthetic,
            job_id: None,
        };
        fs::write(
            dir.join("screenshots").join("index.json"),
            serde_json::to_string(&vec![meta]).unwrap(),
        )
        .unwrap();
        let manifest = faf_ml_core::DatasetManifest {
            name: "v1".to_string(),
            created_at: chrono::Utc::now(),
            entries: vec![faf_ml_core::DatasetEntry {
                image_id: id,
                labels: vec![LabeledBox {
                    class: "bomber".to_string(),
                    x: 100.0,
                    y: 50.0,
                    w: 40.0,
                    h: 20.0,
                }],
            }],
        };
        fs::write(
            dir.join("datasets").join("v1.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        let ds = DetectDataset::load_snapshot(&dir, "v1", 640).unwrap();
        assert_eq!(ds.classes, vec!["tank", "bomber"]);
        assert_eq!(ds.len(), 1);
        assert_eq!(ds.samples[0].gt.len(), 1);
        assert_eq!(ds.samples[0].gt[0].class_id, 1);
        assert!((ds.samples[0].gt[0].bbox.cx - 120.0 / 640.0).abs() < 1e-6);

        // A missing snapshot errors with the "create one first" guidance.
        let err = DetectDataset::load_snapshot(&dir, "nope", 640).unwrap_err();
        assert!(format!("{err:#}").contains("no such snapshot"));

        let _ = fs::remove_dir_all(&dir);
    }
}
