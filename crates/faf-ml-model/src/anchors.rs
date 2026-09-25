//! Anchor grid generation and pure box geometry (IoU, corner↔center, NMS).
//!
//! All functions are host-side f32 math over plain vecs — no tensors — so the
//! conceptual heart of the detector stays unit-testable. Boxes are normalized
//! to 0..=1 image coordinates.

use serde::{Deserialize, Serialize};

/// A box in corner form: (x1, y1) top-left, (x2, y2) bottom-right, normalized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// A box in center form: center + width/height, normalized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CenterBox {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
}

impl CenterBox {
    pub fn to_corner(&self) -> CornerBox {
        CornerBox {
            x1: self.cx - self.w / 2.0,
            y1: self.cy - self.h / 2.0,
            x2: self.cx + self.w / 2.0,
            y2: self.cy + self.h / 2.0,
        }
    }
}

impl CornerBox {
    pub fn to_center(&self) -> CenterBox {
        CenterBox {
            cx: (self.x1 + self.x2) / 2.0,
            cy: (self.y1 + self.y2) / 2.0,
            w: self.x2 - self.x1,
            h: self.y2 - self.y1,
        }
    }

    /// Clamped to the 0..=1 image range (decoded boxes can overshoot).
    pub fn clamped(&self) -> CornerBox {
        let c = |v: f32| v.clamp(0.0, 1.0);
        CornerBox {
            x1: c(self.x1),
            y1: c(self.y1),
            x2: c(self.x2),
            y2: c(self.y2),
        }
    }
}

/// Intersection-over-union of two corner-form boxes (0 if disjoint).
pub fn iou(a: &CornerBox, b: &CornerBox) -> f32 {
    let iw = (a.x2.min(b.x2) - a.x1.max(b.x1)).max(0.0);
    let ih = (a.y2.min(b.y2) - a.y1.max(b.y1)).max(0.0);
    let inter = iw * ih;
    let area = |b: &CornerBox| (b.x2 - b.x1).max(0.0) * (b.y2 - b.y1).max(0.0);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Row-major (anchors × gt) IoU matrix, flattened.
pub fn iou_matrix(anchors: &[CornerBox], gts: &[CornerBox]) -> Vec<f32> {
    let mut out = Vec::with_capacity(anchors.len() * gts.len());
    for a in anchors {
        for g in gts {
            out.push(iou(a, g));
        }
    }
    out
}

/// Greedy non-maximum suppression: returns the indices of kept boxes, in
/// descending score order. `boxes` and `scores` must have equal length.
pub fn nms(boxes: &[CornerBox], scores: &[f32], iou_threshold: f32) -> Vec<usize> {
    assert_eq!(boxes.len(), scores.len());
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    let mut keep: Vec<usize> = Vec::new();
    for &i in &order {
        if keep
            .iter()
            .all(|&k| iou(&boxes[i], &boxes[k]) <= iou_threshold)
        {
            keep.push(i);
        }
    }
    keep
}

/// One scale of the SSD feature pyramid (a square feature map plus the anchor
/// shapes repeated at every cell).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnchorSpec {
    /// Feature map side length (map is `map_size` × `map_size` cells).
    pub map_size: usize,
    /// Anchor side lengths in pixels, before the aspect-ratio scaling.
    pub sizes_px: Vec<f32>,
    /// Aspect ratios (w/h). Combined d2l-style to bound the anchor count:
    /// the first size pairs with every ratio, remaining sizes only with the
    /// first ratio → `sizes.len() + ratios.len() - 1` anchors per cell.
    pub ratios: Vec<f32>,
}

impl AnchorSpec {
    /// Anchors generated per map cell.
    pub fn anchors_per_cell(&self) -> usize {
        self.sizes_px.len() + self.ratios.len() - 1
    }

    /// (w, h) pixel box shapes at one cell, in generation order (anchor
    /// dimension is innermost — must match the head-channel layout in
    /// `model.rs::head_forward`).
    pub fn shapes_px(&self) -> Vec<(f32, f32)> {
        let mut out = Vec::with_capacity(self.anchors_per_cell());
        let ratio_hw = |size: f32, ratio: f32| (size * ratio.sqrt(), size / ratio.sqrt());
        for &r in &self.ratios {
            out.push(ratio_hw(self.sizes_px[0], r));
        }
        for &s in &self.sizes_px[1..] {
            out.push(ratio_hw(s, self.ratios[0]));
        }
        out
    }
}

/// Default anchor pyramid for 640×640 icon crops (icons are ~13–26 px).
///
/// Sizes grow with stride so every icon scale has a well-fitting anchor on
/// at least one map; ratios straddle the sprites' native 36×40 (0.9) shape.
pub fn default_anchor_spec() -> Vec<AnchorSpec> {
    let ratios = || vec![0.9, 1.0, 1.1];
    vec![
        AnchorSpec {
            map_size: 80,
            sizes_px: vec![16.0, 24.0],
            ratios: ratios(),
        },
        AnchorSpec {
            map_size: 40,
            sizes_px: vec![32.0],
            ratios: ratios(),
        },
        AnchorSpec {
            map_size: 20,
            sizes_px: vec![64.0],
            ratios: ratios(),
        },
        AnchorSpec {
            map_size: 10,
            sizes_px: vec![128.0],
            ratios: ratios(),
        },
    ]
}

/// Generate the full anchor grid in head-output order: scales in spec order,
/// cells row-major (y outer, x inner), per-cell anchors innermost.
pub fn generate_anchors(specs: &[AnchorSpec], input_size: u32) -> Vec<CenterBox> {
    let s = input_size as f32;
    let mut anchors = Vec::new();
    for spec in specs {
        let shapes = spec.shapes_px();
        let cell = s / spec.map_size as f32;
        for row in 0..spec.map_size {
            for col in 0..spec.map_size {
                let cx = (col as f32 + 0.5) * cell / s;
                let cy = (row as f32 + 0.5) * cell / s;
                for &(w, h) in &shapes {
                    anchors.push(CenterBox {
                        cx,
                        cy,
                        w: w / s,
                        h: h / s,
                    });
                }
            }
        }
    }
    anchors
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-6;

    fn corner(x1: f32, y1: f32, x2: f32, y2: f32) -> CornerBox {
        CornerBox { x1, y1, x2, y2 }
    }

    #[test]
    fn corner_center_round_trip() {
        let b = corner(0.1, 0.2, 0.5, 0.8);
        let back = b.to_center().to_corner();
        assert!((back.x1 - b.x1).abs() < EPS && (back.y2 - b.y2).abs() < EPS);
        let c = CenterBox {
            cx: 0.3,
            cy: 0.4,
            w: 0.2,
            h: 0.1,
        };
        let back = c.to_corner().to_center();
        assert!((back.cx - c.cx).abs() < EPS && (back.h - c.h).abs() < EPS);
    }

    #[test]
    fn iou_known_values() {
        let a = corner(0.0, 0.0, 0.5, 0.5);
        assert!((iou(&a, &a) - 1.0).abs() < EPS);
        let disjoint = corner(0.6, 0.6, 0.8, 0.8);
        assert_eq!(iou(&a, &disjoint), 0.0);
        // Half-overlap in both dims: inter .25² ... a=[0,.5]² b=[.25,.75]²
        let b = corner(0.25, 0.25, 0.75, 0.75);
        let inter = 0.25 * 0.25;
        let union = 0.25 + 0.25 - inter;
        assert!((iou(&a, &b) - inter / union).abs() < EPS);
    }

    #[test]
    fn nms_suppresses_overlaps_keeps_disjoint() {
        let boxes = vec![
            corner(0.0, 0.0, 0.5, 0.5),     // 0: best score
            corner(0.05, 0.05, 0.55, 0.55), // 1: heavy overlap with 0 → suppressed
            corner(0.6, 0.6, 0.9, 0.9),     // 2: disjoint → kept
        ];
        let scores = vec![0.9, 0.8, 0.7];
        let kept = nms(&boxes, &scores, 0.45);
        assert_eq!(kept, vec![0, 2]);
    }

    #[test]
    fn nms_empty_and_single() {
        assert!(nms(&[], &[], 0.45).is_empty());
        let kept = nms(&[corner(0.0, 0.0, 0.1, 0.1)], &[0.5], 0.45);
        assert_eq!(kept, vec![0]);
    }

    #[test]
    fn anchor_grid_count_and_bounds() {
        let spec = default_anchor_spec();
        let expected: usize = spec
            .iter()
            .map(|s| s.map_size * s.map_size * s.anchors_per_cell())
            .sum();
        let anchors = generate_anchors(&spec, 640);
        assert_eq!(anchors.len(), expected);
        assert_eq!(spec[0].anchors_per_cell(), 4); // 2 sizes + 3 ratios − 1
        for a in &anchors {
            assert!(a.cx > 0.0 && a.cx < 1.0 && a.cy > 0.0 && a.cy < 1.0);
            assert!(a.w > 0.0 && a.h > 0.0);
        }
    }

    #[test]
    fn anchor_coverage_sanity() {
        // A typical 20×20 px icon anywhere on the image must have an anchor
        // with IoU ≥ 0.4 (≥0.5 near cell centers; the forced best-anchor
        // match in matching.rs covers the worst case).
        let anchors = generate_anchors(&default_anchor_spec(), 640);
        let corners: Vec<CornerBox> = anchors.iter().map(|a| a.to_corner()).collect();
        let s = 640.0f32;
        let mut worst = 1.0f32;
        for gy in (10..630).step_by(25) {
            for gx in (10..630).step_by(25) {
                let icon = CenterBox {
                    cx: gx as f32 / s,
                    cy: gy as f32 / s,
                    w: 20.0 / s,
                    h: 20.0 / s,
                }
                .to_corner();
                let best = corners.iter().map(|a| iou(a, &icon)).fold(0.0, f32::max);
                worst = worst.min(best);
            }
        }
        assert!(worst >= 0.4, "worst best-anchor IoU for 20px icon: {worst}");
    }
}
