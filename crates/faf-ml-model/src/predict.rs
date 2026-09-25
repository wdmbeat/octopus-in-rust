//! Inference: forward → softmax → decode offsets → score threshold →
//! per-class NMS, plus a minimal annotated-preview renderer.

use std::path::Path;

use anyhow::{Context, Result};
use burn::tensor::activation::softmax;
use burn::tensor::backend::Backend;
use burn::tensor::Tensor;
use image::{Rgb, RgbImage};

use crate::anchors::{nms, CenterBox, CornerBox};
use crate::matching::decode_offsets;
use crate::model::SsdModel;

/// One detection after decoding + NMS.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// 0-based index into `DetectorConfig::classes`.
    pub class_id: usize,
    pub score: f32,
    /// Normalized corner-form box (clamped to 0..=1).
    pub bbox: CornerBox,
}

/// Run the detector on one image tensor (1, 3, H, W).
///
/// Returns detections sorted by descending score. `score_threshold` applies
/// to the best non-background class probability per anchor; survivors go
/// through per-class NMS at `nms_iou`.
pub fn predict<B: Backend>(
    model: &SsdModel<B>,
    anchors: &[CenterBox],
    image: Tensor<B, 4>,
    score_threshold: f32,
    nms_iou: f32,
) -> Vec<Detection> {
    let (cls_logits, box_preds) = model.forward(image);
    let [_, n_anchors, _] = cls_logits.dims();
    debug_assert_eq!(n_anchors, anchors.len());

    let probs: Vec<f32> = softmax(cls_logits, 2)
        .into_data()
        .to_vec()
        .expect("cls probs readback");
    let offsets: Vec<f32> = box_preds.into_data().to_vec().expect("box readback");
    let num_classes = probs.len() / n_anchors - 1; // includes background slot

    // Per-class candidate lists (class c holds anchors whose best non-bg
    // class is c and that pass the score threshold).
    let mut per_class: Vec<(Vec<CornerBox>, Vec<f32>)> =
        vec![(Vec::new(), Vec::new()); num_classes];
    for a in 0..n_anchors {
        let row = &probs[a * (num_classes + 1)..(a + 1) * (num_classes + 1)];
        let (best_c, &best_p) = row[1..]
            .iter()
            .enumerate()
            .max_by(|(_, x), (_, y)| x.total_cmp(y))
            .expect("at least one foreground class");
        if best_p < score_threshold {
            continue;
        }
        let o = &offsets[a * 4..a * 4 + 4];
        let bbox = decode_offsets(&anchors[a], [o[0], o[1], o[2], o[3]])
            .to_corner()
            .clamped();
        per_class[best_c].0.push(bbox);
        per_class[best_c].1.push(best_p);
    }

    let mut detections = Vec::new();
    for (class_id, (boxes, scores)) in per_class.into_iter().enumerate() {
        for kept in nms(&boxes, &scores, nms_iou) {
            detections.push(Detection {
                class_id,
                score: scores[kept],
                bbox: boxes[kept],
            });
        }
    }
    detections.sort_by(|a, b| b.score.total_cmp(&a.score));
    detections
}

/// Draw detection boxes onto an image (thin 1-px rects, green; the same
/// minimal style as datagen's old `draw_preview`). Labels are printed by the
/// caller — no text rendering here.
pub fn draw_detections(img: &RgbImage, detections: &[Detection]) -> RgbImage {
    let mut out = img.clone();
    let green = Rgb([0, 255, 0]);
    let w = out.width();
    let h = out.height();
    for det in detections {
        let x1 = (det.bbox.x1 * w as f32) as u32;
        let y1 = (det.bbox.y1 * h as f32) as u32;
        let x2 = ((det.bbox.x2 * w as f32) as u32).min(w - 1);
        let y2 = ((det.bbox.y2 * h as f32) as u32).min(h - 1);
        for x in x1..=x2 {
            out.put_pixel(x, y1, green);
            out.put_pixel(x, y2, green);
        }
        for y in y1..=y2 {
            out.put_pixel(x1, y, green);
            out.put_pixel(x2, y, green);
        }
    }
    out
}

/// Load an image for preview rendering (RGB8).
pub fn load_rgb(path: &Path) -> Result<RgbImage> {
    Ok(image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_rgb8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_detections_paints_box_edges() {
        let img = RgbImage::from_pixel(100, 100, Rgb([0, 0, 0]));
        let det = Detection {
            class_id: 0,
            score: 0.9,
            bbox: CornerBox {
                x1: 0.1,
                y1: 0.1,
                x2: 0.5,
                y2: 0.5,
            },
        };
        let out = draw_detections(&img, &[det]);
        assert_eq!(*out.get_pixel(10, 10), Rgb([0, 255, 0])); // corner
        assert_eq!(*out.get_pixel(30, 10), Rgb([0, 255, 0])); // top edge
        assert_eq!(*out.get_pixel(30, 30), Rgb([0, 0, 0])); // interior untouched
    }
}
