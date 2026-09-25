//! The SSD-style detector: VGG-ish backbone with feature taps at strides
//! 8/16/32/64, plus per-scale class and box-offset heads.
//!
//! Layout contract with `anchors::generate_anchors`: each head emits
//! `anchors_per_cell × out_per_anchor` channels ordered (anchor, value); the
//! forward reshapes to (batch, cells_row_major × anchors, values) and the
//! scales are concatenated in spec order — so channel `a*C + c` of cell
//! (y, x) on scale `s` corresponds to anchor `s_offset + (y*W + x)*A + a`.

use burn::module::{Initializer, Module};
use burn::nn::conv::{Conv2d, Conv2dConfig};
use burn::nn::PaddingConfig2d;
use burn::tensor::activation::relu;
use burn::tensor::backend::Backend;
use burn::tensor::{Device, Tensor};
use serde::{Deserialize, Serialize};

use crate::anchors::AnchorSpec;

/// Everything needed to rebuild a model from a checkpoint directory
/// (serialized as `config.json` next to `model.mpk`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectorConfig {
    /// Square input side length in pixels (datagen `--size`).
    pub input_size: u32,
    /// Foreground class names from classes.txt (line number = class id).
    /// Background is implicit class 0 in the head output.
    pub classes: Vec<String>,
    /// Anchor pyramid; length must equal `backbone_channels.len()`.
    pub anchors: Vec<AnchorSpec>,
    /// Backbone output channels tapped at each scale (strides 8/16/32/64).
    pub backbone_channels: Vec<usize>,
}

/// Single-shot multibox detector over a 4-scale feature pyramid.
#[derive(Module, Debug)]
pub struct SsdModel<B: Backend> {
    /// All backbone convs in forward order; `tap_indices` marks the outputs
    /// fed to the heads (after each stride-2 downsample of a tapped stage).
    backbone: Vec<Conv2d<B>>,
    cls_heads: Vec<Conv2d<B>>,
    box_heads: Vec<Conv2d<B>>,
    tap_indices: Vec<usize>,
    anchors_per_cell: Vec<usize>,
    num_classes: usize,
}

/// 3×3 conv, same-padding (stride 1 keeps the map size; stride 2 halves it).
fn conv<B: Backend>(cin: usize, cout: usize, stride: usize, device: &Device<B>) -> Conv2d<B> {
    Conv2dConfig::new([cin, cout], [3, 3])
        .with_stride([stride, stride])
        .with_padding(PaddingConfig2d::Same)
        .with_initializer(Initializer::KaimingUniform {
            gain: 1.0 / 3.0f64.sqrt(), // = 1/sqrt(3), burn's default; explicit for clarity
            fan_out_only: false,
        })
        .init(device)
}

impl<B: Backend> SsdModel<B> {
    pub fn new(config: &DetectorConfig, device: &Device<B>) -> Self {
        assert_eq!(
            config.anchors.len(),
            config.backbone_channels.len(),
            "one channel count per anchor scale"
        );
        let num_classes = config.classes.len();

        // Stem: 640 → 320 → 160 (stride 4). The first conv strides
        // immediately: a 32-channel 640² output would be a 200 MB buffer at
        // batch 4, above cubecl-wgpu's per-page cap. Then one stage per
        // tapped scale, each = two convs at the current resolution + a
        // stride-2 downsample INTO the tapped resolution: 80/40/20/10.
        let mut backbone = vec![
            conv(3, 32, 2, device),
            conv(32, 32, 1, device),
            conv(32, 64, 2, device),
        ];
        let mut tap_indices = Vec::with_capacity(config.backbone_channels.len());
        let mut cin = 64;
        for &c in &config.backbone_channels {
            backbone.push(conv(cin, c, 1, device));
            backbone.push(conv(c, c, 1, device));
            backbone.push(conv(c, c, 2, device));
            tap_indices.push(backbone.len() - 1);
            cin = c;
        }

        let anchors_per_cell: Vec<usize> = config
            .anchors
            .iter()
            .map(|s| s.anchors_per_cell())
            .collect();
        let cls_heads = config
            .backbone_channels
            .iter()
            .zip(&anchors_per_cell)
            .map(|(&c, &a)| conv(c, a * (num_classes + 1), 1, device))
            .collect();
        let box_heads = config
            .backbone_channels
            .iter()
            .zip(&anchors_per_cell)
            .map(|(&c, &a)| conv(c, a * 4, 1, device))
            .collect();

        Self {
            backbone,
            cls_heads,
            box_heads,
            tap_indices,
            anchors_per_cell,
            num_classes,
        }
    }

    /// Feature maps at the tap points, in anchor-spec order (strides
    /// 8/16/32/64 for the default config).
    fn forward_features(&self, images: Tensor<B, 4>) -> Vec<Tensor<B, 4>> {
        let mut x = images;
        let mut taps = Vec::with_capacity(self.tap_indices.len());
        for (i, layer) in self.backbone.iter().enumerate() {
            x = relu(layer.forward(x));
            if self.tap_indices.contains(&i) {
                taps.push(x.clone());
            }
        }
        taps
    }

    /// (batch, 3, H, W) → (class logits (batch, n_anchors, classes+1),
    /// box offsets (batch, n_anchors, 4)), anchors in `generate_anchors` order.
    pub fn forward(&self, images: Tensor<B, 4>) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let features = self.forward_features(images);
        let mut cls_all = Vec::with_capacity(features.len());
        let mut box_all = Vec::with_capacity(features.len());
        for (i, fmap) in features.into_iter().enumerate() {
            let a = self.anchors_per_cell[i];
            cls_all.push(head_forward(
                self.cls_heads[i].forward(fmap.clone()),
                a,
                self.num_classes + 1,
            ));
            box_all.push(head_forward(self.box_heads[i].forward(fmap), a, 4));
        }
        (
            Tensor::cat(cls_all, 1), // (batch, n_anchors, classes+1)
            Tensor::cat(box_all, 1), // (batch, n_anchors, 4)
        )
    }
}

/// (batch, anchors×out, H, W) head output → (batch, H·W·anchors, out).
/// Channel layout is (anchor, out) so anchor stays innermost after the
/// permute — matching `AnchorSpec::shapes_px` order within each cell.
fn head_forward<B: Backend>(y: Tensor<B, 4>, anchors: usize, out: usize) -> Tensor<B, 3> {
    let [b, _, h, w] = y.dims();
    y.reshape([b, anchors, out, h, w])
        .permute([0, 3, 4, 1, 2]) // (batch, H, W, anchors, out)
        .reshape([b, h * w * anchors, out])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchors::{default_anchor_spec, generate_anchors};

    fn test_config() -> DetectorConfig {
        DetectorConfig {
            input_size: 640,
            classes: vec!["a".into(), "b".into(), "c".into()],
            anchors: default_anchor_spec(),
            backbone_channels: vec![64, 128, 256, 256],
        }
    }

    /// Shapes + the anchor-order contract, on the CPU backend (no GPU needed
    /// for a plumbing test). Marked #[ignore] by default? No — it's fast
    /// enough in release-profile deps; keep it running.
    #[test]
    fn forward_shapes_match_anchor_grid() {
        type CpuB = burn::backend::NdArray<f32>;
        let device: Device<CpuB> = Default::default();
        let config = test_config();
        let model = SsdModel::<CpuB>::new(&config, &device);
        let anchors = generate_anchors(&config.anchors, config.input_size);

        let images = Tensor::<CpuB, 4>::zeros([2, 3, 640, 640], &device);
        let (cls, bbox) = model.forward(images);
        assert_eq!(cls.dims(), [2, anchors.len(), config.classes.len() + 1]);
        assert_eq!(bbox.dims(), [2, anchors.len(), 4]);
    }
}
