//! SSD training loss: cross-entropy over ALL anchors (background = class 0)
//! plus L1 box regression masked to positive anchors only (d2l §14.7).
//! Total = cls + bbox (the book's weighting).

use burn::nn::loss::CrossEntropyLossConfig;
use burn::tensor::backend::Backend;
use burn::tensor::{Device, Int, Tensor};

/// The two loss terms plus their sum, kept separate for per-epoch logging.
pub struct LossComponents<B: Backend> {
    pub total: Tensor<B, 1>,
    pub cls: Tensor<B, 1>,
    pub bbox: Tensor<B, 1>,
}

/// * `cls_logits`: (batch, n_anchors, classes+1) raw logits
/// * `box_preds`:  (batch, n_anchors, 4) encoded offsets
/// * `cls_targets`: (batch, n_anchors) int — 0 = background
/// * `box_targets`: (batch, n_anchors, 4) encoded GT offsets
/// * `pos_mask`:   (batch, n_anchors) float 0/1 — 1 on matched anchors
pub fn ssd_loss<B: Backend>(
    cls_logits: Tensor<B, 3>,
    box_preds: Tensor<B, 3>,
    cls_targets: Tensor<B, 2, Int>,
    box_targets: Tensor<B, 3>,
    pos_mask: Tensor<B, 2>,
    device: &Device<B>,
) -> LossComponents<B> {
    let [b, n, c] = cls_logits.dims();

    // Classification: mean CE over every anchor, positives and background.
    let cls = CrossEntropyLossConfig::new()
        .init(device)
        .forward(cls_logits.reshape([b * n, c]), cls_targets.reshape([b * n]));

    // Box regression: L1 over positive anchors only. The mask is unsqueezed
    // to rank 3 first — Wgpu only broadcasts across EQUAL ranks.
    let masked_l1 = (box_preds - box_targets).abs() * pos_mask.clone().unsqueeze_dim::<3>(2);
    let n_pos_offsets = pos_mask.sum().clamp_min(1.0) * 4.0;
    let bbox = masked_l1.sum() / n_pos_offsets;

    let total = cls.clone() + bbox.clone();
    LossComponents { total, cls, bbox }
}
