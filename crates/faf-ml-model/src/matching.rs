//! Anchor ↔ ground-truth assignment and box-offset encoding (d2l §14.4/§14.7).
//!
//! Pure host-side index math over vecs — run per training batch on the CPU,
//! then uploaded as target tensors. Unit-tested here; the loss in `loss.rs`
//! only consumes the resulting targets.

use crate::anchors::{iou_matrix, CenterBox, CornerBox};

/// Center-form offset encoding scales (d2l §14.4: variances 0.1 / 0.2).
const CENTER_SCALE: f32 = 10.0; // center offsets are divided by 0.1
const SIZE_SCALE: f32 = 5.0; // log-size offsets are divided by 0.2

/// Result of assigning every anchor to at most one ground-truth box.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorMatch {
    /// `anchor_to_gt[i]` = index of the GT box matched to anchor `i`,
    /// or `None` for a background anchor.
    pub anchor_to_gt: Vec<Option<usize>>,
}

/// Assign anchors to GT boxes: an anchor is positive when its best IoU with
/// any GT reaches `pos_threshold`; additionally every GT is force-matched to
/// its single best anchor so no GT is ever left unmatched.
pub fn match_anchors(
    anchors: &[CornerBox],
    gt_boxes: &[CornerBox],
    pos_threshold: f32,
) -> AnchorMatch {
    let n = anchors.len();
    let g = gt_boxes.len();
    let mut anchor_to_gt = vec![None; n];
    if g == 0 {
        return AnchorMatch { anchor_to_gt };
    }

    let ious = iou_matrix(anchors, gt_boxes); // row-major n × g

    // Threshold pass: anchor → its best GT if IoU is high enough.
    for a in 0..n {
        let (best_gt, best_iou) =
            (0..g)
                .map(|gt| (gt, ious[a * g + gt]))
                .fold(
                    (0, f32::MIN),
                    |best, cur| if cur.1 > best.1 { cur } else { best },
                );
        if best_iou >= pos_threshold {
            anchor_to_gt[a] = Some(best_gt);
        }
    }

    // Force-match pass: every GT keeps at least its best anchor.
    for gt in 0..g {
        if (0..n).any(|a| anchor_to_gt[a] == Some(gt)) {
            continue;
        }
        let best_anchor = (0..n).map(|a| (a, ious[a * g + gt])).fold(0, |best, cur| {
            if cur.1 > ious[best * g + gt] {
                cur.0
            } else {
                best
            }
        });
        anchor_to_gt[best_anchor] = Some(gt);
    }

    AnchorMatch { anchor_to_gt }
}

/// Encode a GT box as offsets from its matched anchor (center form, scaled).
pub fn encode_offsets(anchor: &CenterBox, gt: &CenterBox) -> [f32; 4] {
    [
        (gt.cx - anchor.cx) / anchor.w * CENTER_SCALE,
        (gt.cy - anchor.cy) / anchor.h * CENTER_SCALE,
        (gt.w / anchor.w).ln() * SIZE_SCALE,
        (gt.h / anchor.h).ln() * SIZE_SCALE,
    ]
}

/// Decode predicted offsets back into a box (inverse of `encode_offsets`).
pub fn decode_offsets(anchor: &CenterBox, offsets: [f32; 4]) -> CenterBox {
    CenterBox {
        cx: offsets[0] / CENTER_SCALE * anchor.w + anchor.cx,
        cy: offsets[1] / CENTER_SCALE * anchor.h + anchor.cy,
        w: (offsets[2] / SIZE_SCALE).exp() * anchor.w,
        h: (offsets[3] / SIZE_SCALE).exp() * anchor.h,
    }
}

/// Per-image training targets over the full anchor grid, ready to upload.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorTargets {
    /// Class target per anchor: 0 = background, otherwise `class_id + 1`.
    pub cls: Vec<i64>,
    /// Encoded offsets per anchor (zeros for background anchors).
    pub offsets: Vec<[f32; 4]>,
    /// 1.0 for positive (matched) anchors, 0.0 for background.
    pub pos_mask: Vec<f32>,
}

impl AnchorTargets {
    /// Build targets for one image: match GT boxes to anchors, then encode.
    /// `gt` pairs a class id (0-based, into classes.txt) with a normalized box.
    pub fn build(
        anchors: &[CenterBox],
        gt: &[(usize, CenterBox)],
        pos_threshold: f32,
    ) -> AnchorTargets {
        let anchor_corners: Vec<CornerBox> = anchors.iter().map(|a| a.to_corner()).collect();
        let gt_corners: Vec<CornerBox> = gt.iter().map(|(_, b)| b.to_corner()).collect();
        let m = match_anchors(&anchor_corners, &gt_corners, pos_threshold);

        let mut cls = vec![0i64; anchors.len()];
        let mut offsets = vec![[0.0; 4]; anchors.len()];
        let mut pos_mask = vec![0.0f32; anchors.len()];
        for (i, assigned) in m.anchor_to_gt.iter().enumerate() {
            if let Some(gt_idx) = assigned {
                cls[i] = gt[*gt_idx].0 as i64 + 1;
                offsets[i] = encode_offsets(&anchors[i], &gt[*gt_idx].1);
                pos_mask[i] = 1.0;
            }
        }
        AnchorTargets {
            cls,
            offsets,
            pos_mask,
        }
    }

    pub fn num_positive(&self) -> usize {
        self.pos_mask.iter().filter(|&&m| m > 0.0).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn center(cx: f32, cy: f32, w: f32, h: f32) -> CenterBox {
        CenterBox { cx, cy, w, h }
    }

    #[test]
    fn offset_encode_decode_round_trip() {
        let anchor = center(0.30, 0.40, 0.05, 0.06);
        let gt = center(0.34, 0.36, 0.08, 0.04);
        let encoded = encode_offsets(&anchor, &gt);
        let decoded = decode_offsets(&anchor, encoded);
        assert!((decoded.cx - gt.cx).abs() < EPS);
        assert!((decoded.cy - gt.cy).abs() < EPS);
        assert!((decoded.w - gt.w).abs() < EPS);
        assert!((decoded.h - gt.h).abs() < EPS);
    }

    #[test]
    fn decode_of_zero_offsets_is_anchor() {
        let anchor = center(0.5, 0.5, 0.1, 0.1);
        assert_eq!(decode_offsets(&anchor, [0.0; 4]), anchor);
    }

    #[test]
    fn matching_threshold_and_background() {
        let anchors = [
            center(0.5, 0.5, 0.1, 0.1), // overlaps the GT
            center(0.9, 0.9, 0.1, 0.1), // far away → background
        ];
        let gt = [center(0.52, 0.51, 0.1, 0.1)];
        let anchor_corners: Vec<CornerBox> = anchors.iter().map(|a| a.to_corner()).collect();
        let gt_corners: Vec<CornerBox> = gt.iter().map(|a| a.to_corner()).collect();
        let m = match_anchors(&anchor_corners, &gt_corners, 0.5);
        assert_eq!(m.anchor_to_gt[0], Some(0));
        assert_eq!(m.anchor_to_gt[1], None);
    }

    #[test]
    fn matching_force_matches_best_anchor_below_threshold() {
        // GT only weakly overlaps anchor 0 (IoU < 0.5); it must still be
        // force-matched so every GT supervises exactly its best anchor.
        let anchors = [center(0.50, 0.50, 0.10, 0.10)];
        let gt = [center(0.56, 0.50, 0.10, 0.10)]; // IoU = 0.04/0.16 = 0.25
        let m = match_anchors(&[anchors[0].to_corner()], &[gt[0].to_corner()], 0.5);
        assert_eq!(m.anchor_to_gt[0], Some(0));
    }

    #[test]
    fn matching_no_gt_is_all_background() {
        let anchors = [center(0.5, 0.5, 0.1, 0.1)];
        let m = match_anchors(&[anchors[0].to_corner()], &[], 0.5);
        assert_eq!(m.anchor_to_gt, vec![None]);
    }

    #[test]
    fn build_targets_labels_and_masks() {
        let anchors = vec![center(0.5, 0.5, 0.1, 0.1), center(0.9, 0.9, 0.1, 0.1)];
        let gt = vec![(7usize, center(0.51, 0.50, 0.1, 0.1))];
        let t = AnchorTargets::build(&anchors, &gt, 0.5);
        assert_eq!(t.cls[0], 8); // class id + 1 (0 = background)
        assert_eq!(t.cls[1], 0);
        assert_eq!(t.pos_mask, vec![1.0, 0.0]);
        assert_eq!(t.num_positive(), 1);
        // Positive offsets encode the GT; background offsets stay zero.
        assert!(t.offsets[0].iter().any(|v| v.abs() > EPS));
        assert_eq!(t.offsets[1], [0.0; 4]);
    }
}
